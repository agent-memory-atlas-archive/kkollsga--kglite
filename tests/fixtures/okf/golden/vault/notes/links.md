---
title: Link semantics
---
Above the first heading a link carries no section: see [[Roadmap]].

## Deep dive

[[Seismic interpretation]] resolves through an alias, [[atlas#Overview]]
carries an anchor, and ![[old]] embeds a whole note.

## Figures

![Fault map](../img/faults.png) resolves note-relative and carries an alt;
![[diagram.png]] resolves on the bare-filename rung with none. The appendix
![missing appendix](../img/appendix.pdf) is referenced but not present.

## Related topics

[[roadmap]] sits under a heading the built-in ladder reads as `RELATED`; a
vault declaring `heading_edges` retypes the same link without touching it.

## Gallery ![in a heading](../img/faults.png) beside [[atlas]]

A heading line is a body line: the picture and the link written *in* it are
both extracted, and both carry the whole heading as their `section`.
