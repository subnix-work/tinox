package tinox.eclipse;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileReader;
import java.io.IOException;

/**
 * Minimal tinox.toml reader -- mirrors crates/tinox/src/pm.rs's own
 * hand-rolled line-scanning parser (not a real TOML library there
 * either) rather than pulling in a TOML dependency for one field.
 */
public class TinoxToml {

    /**
     * Reads [package] name from tomlFile. Returns null (not the empty
     * string) when the file has no [package] section or no name key --
     * callers fall back to the parent directory's name in that case,
     * same as pm.rs's own manifest reading never hard-fails on merely
     * incomplete metadata.
     */
    public static String parsePackageName(File tomlFile) throws IOException {
        boolean inPackageSection = false;
        try (BufferedReader reader = new BufferedReader(new FileReader(tomlFile))) {
            String line;
            while ((line = reader.readLine()) != null) {
                String trimmed = line.trim();
                if (trimmed.startsWith("[") && !trimmed.startsWith("[[")) {
                    inPackageSection = trimmed.equals("[package]");
                    continue;
                }
                if (!inPackageSection) {
                    continue;
                }
                if (trimmed.startsWith("name")) {
                    String rest = trimmed.substring(4).trim();
                    if (rest.startsWith("=")) {
                        String value = rest.substring(1).trim();
                        value = stripQuotes(value);
                        if (!value.isEmpty()) {
                            return value;
                        }
                    }
                }
            }
        }
        return null;
    }

    private static String stripQuotes(String value) {
        if (value.length() >= 2 && value.charAt(0) == '"' && value.charAt(value.length() - 1) == '"') {
            return value.substring(1, value.length() - 1);
        }
        return value;
    }

    private TinoxToml() {
    }
}
