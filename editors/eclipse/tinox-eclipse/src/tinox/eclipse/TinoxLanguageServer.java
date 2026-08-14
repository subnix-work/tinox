package tinox.eclipse;

import java.util.List;
import org.eclipse.lsp4e.server.ProcessStreamConnectionProvider;

public class TinoxLanguageServer extends ProcessStreamConnectionProvider {

    public static final String PREF_BINARY_PATH = "tinox.lsp.path";

    public TinoxLanguageServer() {
        String binaryPath = Activator.getDefault()
                .getPreferenceStore()
                .getString(PREF_BINARY_PATH);

        setCommands(List.of(binaryPath));
        setWorkingDirectory(System.getProperty("user.home"));
    }
}
