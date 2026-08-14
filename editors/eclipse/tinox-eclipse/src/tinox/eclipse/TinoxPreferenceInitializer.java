package tinox.eclipse;

import org.eclipse.core.runtime.preferences.AbstractPreferenceInitializer;
import org.eclipse.jface.preference.IPreferenceStore;

public class TinoxPreferenceInitializer extends AbstractPreferenceInitializer {

    @Override
    public void initializeDefaultPreferences() {
        IPreferenceStore store = Activator.getDefault().getPreferenceStore();

        // Default: tinox-lsp on the PATH, if installed
        String defaultPath = findDefaultBinaryPath();
        store.setDefault(TinoxLanguageServer.PREF_BINARY_PATH, defaultPath);
    }

    private String findDefaultBinaryPath() {
        // Try common paths
        String home = System.getProperty("user.home");
        String[] candidates = {
            home + "/.cargo/bin/tinox-lsp",
            "/usr/local/bin/tinox-lsp",
            "/usr/bin/tinox-lsp",
        };
        for (String path : candidates) {
            if (new java.io.File(path).canExecute()) {
                return path;
            }
        }
        return home + "/.cargo/bin/tinox-lsp";
    }
}
