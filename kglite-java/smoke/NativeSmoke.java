import io.github.kkollsga.kglite.KnowledgeGraph;
import java.util.List;
import java.util.Map;

/**
 * Loads one freshly built native and runs {@code RETURN 1} through it.
 *
 * <p>Run by {@code .github/workflows/publish_java.yml} on each platform's own
 * runner, straight after that platform's {@code cargo build -p kglite-c}, with
 * {@code -Dkglite.native.path} naming the library just built. The JAR's own
 * test suite runs on Linux only, so this is the one place the darwin, Windows
 * and linux-aarch64 natives are ever loaded before they ship. Launched as a
 * source file against the compiled wrapper classes; not part of the Gradle
 * build.
 */
public final class NativeSmoke {
    private NativeSmoke() {}

    public static void main(String[] args) {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            List<Map<String, Object>> rows = graph.cypher("RETURN 1 AS x");
            Object x = rows.size() == 1 ? rows.get(0).get("x") : null;
            if (!(x instanceof Number n) || n.longValue() != 1L) {
                throw new AssertionError("RETURN 1 AS x returned " + rows);
            }
            System.out.println("kglite native " + KnowledgeGraph.nativeAbiVersion()
                    + " on " + System.getProperty("os.name") + "/" + System.getProperty("os.arch")
                    + ", Java " + Runtime.version() + ": RETURN 1 -> " + x);
        }
    }
}
