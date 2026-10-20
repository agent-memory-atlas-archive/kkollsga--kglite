//! Serialising a store to the packed byte layout (`.kgl` column sections and the
//! disk sidecars); the reader is [`ColumnStore::load_packed`].

use super::*;

impl ColumnStore {
    /// Serialize all columns to a packed byte buffer for the v3 file format.
    ///
    /// Format per column:
    ///   [2B] col_name_len  [NB] col_name_utf8
    ///   [2B] type_tag_len  [NB] type_tag
    ///   [8B] data_len      [NB] data_bytes (+ null_bytes for typed columns)
    ///   For "string": data_bytes = offsets + str_data + null_bitmap
    ///   For "mixed": data_bytes = the selected codec's Vec<Value>
    ///   For "int64d" (`.kgl` v6 only): data_bytes = zigzag-varint deltas +
    ///   null_bytes — see [`encode_int64_delta_if_smaller`].
    ///
    /// Emits fixed-width integer columns only. The `.kgl` v6 writer calls
    /// [`Self::write_packed_with_codec`] with [`IntColumnEncoding::Auto`]
    /// instead; every other consumer of this layout (the disk-graph column
    /// sidecars) keeps the fixed-width integer form. A sidecar can still hold a
    /// `"timestamp"` column, which 0.19.0 and earlier do not know. Measured, that
    /// reader never gets that far: it reads `id_indices.bin` first and refuses a
    /// directory this layout wrote with `unsupported raw index version`; read
    /// past that, the column fails as `invalid packed column store: codec error`.
    pub fn write_packed(&self, interner: &StringInterner) -> io::Result<Vec<u8>> {
        self.write_packed_with_codec(
            interner,
            crate::serde_codec::CURRENT_CODEC,
            IntColumnEncoding::Raw,
        )
    }

    pub(crate) fn write_packed_with_codec(
        &self,
        interner: &StringInterner,
        codec: crate::serde_codec::CodecVersion,
        int_encoding: IntColumnEncoding,
    ) -> io::Result<Vec<u8>> {
        self.write_packed_inner(interner, codec, int_encoding, None)
    }

    /// [`Self::write_packed_with_codec`] for a `.kgl` v9 save: columns `split`
    /// claims are written as `@map` entries, and their buffers (borrowed from
    /// this store) are left in `split` for the container writer.
    pub(crate) fn write_packed_split<'a>(
        &'a self,
        interner: &StringInterner,
        codec: crate::serde_codec::CodecVersion,
        int_encoding: IntColumnEncoding,
        split: &mut ColumnSplit<'a>,
    ) -> io::Result<Vec<u8>> {
        self.write_packed_inner(interner, codec, int_encoding, Some(split))
    }

    fn write_packed_inner<'a>(
        &'a self,
        interner: &StringInterner,
        codec: crate::serde_codec::CodecVersion,
        int_encoding: IntColumnEncoding,
        mut split: Option<&mut ColumnSplit<'a>>,
    ) -> io::Result<Vec<u8>> {
        if let Some(ref mmap_store) = self.mmap_store {
            return self.write_packed_from_mmap(mmap_store, interner, codec);
        }

        let mut buf: Vec<u8> = Vec::new();
        let overflow = self.effective_overflow_bytes();

        // Write ALL schema columns (including empty ones) to preserve metadata round-trip.
        // Empty columns are cheap — just type tag + zero-length data blob.
        let extra = self.id_column.is_some() as u32
            + self.title_column.is_some() as u32
            + if overflow.is_some() { 2 } else { 0 };
        let num_cols = self.columns.len() as u32 + extra;
        buf.extend_from_slice(&num_cols.to_le_bytes());

        for (slot, ik) in self.schema.iter() {
            let col_name = interner.resolve(ik);
            let col = &*self.columns[slot as usize];
            if col.len() < self.row_count as usize {
                // Schema growth and mmap-to-owned mutation can leave a typed
                // column shorter than the store. Persist a dense, null-padded
                // view; otherwise the framed row_count makes reload over-read
                // the shorter blob and reject the newly published generation.
                let mut padded = col.clone();
                while padded.len() < self.row_count as usize {
                    padded.push_null();
                }
                Self::write_packed_column(&mut buf, col_name, &padded, codec, int_encoding, None)?;
            } else {
                Self::write_packed_column(
                    &mut buf,
                    col_name,
                    col,
                    codec,
                    int_encoding,
                    split.as_deref_mut(),
                )?;
            }
        }

        for (name, col) in [
            ("__id__", self.id_column.as_deref()),
            ("__title__", self.title_column.as_deref()),
        ] {
            let Some(col) = col else { continue };
            if col.len() < self.row_count as usize {
                let mut padded = col.clone();
                while padded.len() < self.row_count as usize {
                    padded.push_null();
                }
                Self::write_packed_column(&mut buf, name, &padded, codec, int_encoding, None)?;
            } else {
                Self::write_packed_column(
                    &mut buf,
                    name,
                    col,
                    codec,
                    int_encoding,
                    split.as_deref_mut(),
                )?;
            }
        }

        Self::write_overflow_columns(&mut buf, overflow.as_ref());

        Ok(buf)
    }

    pub(super) fn write_packed_column<'a>(
        buf: &mut Vec<u8>,
        col_name: &str,
        col: &'a TypedColumn,
        codec: crate::serde_codec::CodecVersion,
        int_encoding: IntColumnEncoding,
        split: Option<&mut ColumnSplit<'a>>,
    ) -> io::Result<()> {
        // A v6 writer may swap an `Int64` column's fixed-width array for the
        // delta-varint form when that is smaller. The choice is recorded in the
        // per-column type tag, so the reader needs no side channel and a column
        // that declines the swap is byte-identical to what v5 wrote.
        let delta_blob = match (int_encoding, col) {
            (IntColumnEncoding::Auto, TypedColumn::Int64 { data, nulls }) => {
                encode_int64_delta_if_smaller(data, nulls)
            }
            _ => None,
        };
        // A column the split claims is written as a data-region section unless
        // the delta form is at least four times smaller: raw sections are the
        // only mappable ones, so a modest delta saving does not justify
        // decoding the column onto the heap at every load.
        if let Some(split) = split {
            let delta_wins = delta_blob
                .as_ref()
                .is_some_and(|blob| blob.len().saturating_mul(4) <= col.len().saturating_mul(9));
            if !delta_wins && split.claim(col_name, col) {
                let tag = format!("{}{MAP_TAG_SUFFIX}", col.type_tag());
                buf.extend_from_slice(&(col_name.len() as u16).to_le_bytes());
                buf.extend_from_slice(col_name.as_bytes());
                buf.extend_from_slice(&(tag.len() as u16).to_le_bytes());
                buf.extend_from_slice(tag.as_bytes());
                buf.extend_from_slice(&0u64.to_le_bytes());
                return Ok(());
            }
        }
        let type_tag = match delta_blob {
            Some(_) => INT64_DELTA_TAG,
            None => col.type_tag(),
        };

        let name_bytes = col_name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);

        let tag_bytes = type_tag.as_bytes();
        buf.extend_from_slice(&(tag_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(tag_bytes);

        let len_offset = buf.len();
        buf.extend_from_slice(&0u64.to_le_bytes());
        match delta_blob {
            Some(blob) => buf.extend_from_slice(&blob),
            None => col.write_to_with_codec(buf, codec)?,
        }
        let data_len = (buf.len() - len_offset - 8) as u64;
        buf[len_offset..len_offset + 8].copy_from_slice(&data_len.to_le_bytes());
        Ok(())
    }
}
