package tinox.eclipse;

import java.io.InputStream;
import org.eclipse.core.commands.AbstractHandler;
import org.eclipse.core.commands.ExecutionEvent;
import org.eclipse.core.commands.ExecutionException;
import org.eclipse.core.resources.IFile;
import org.eclipse.jface.viewers.ISelection;
import org.eclipse.jface.viewers.IStructuredSelection;
import org.eclipse.ui.IEditorInput;
import org.eclipse.ui.IFileEditorInput;
import org.eclipse.ui.console.ConsolePlugin;
import org.eclipse.ui.console.IConsole;
import org.eclipse.ui.console.IConsoleManager;
import org.eclipse.ui.console.MessageConsole;
import org.eclipse.ui.console.MessageConsoleStream;
import org.eclipse.ui.handlers.HandlerUtil;

public class RunTinoxHandler extends AbstractHandler {

    @Override
    public Object execute(ExecutionEvent event) throws ExecutionException {
        IFile file = getSelectedFile(event);

        MessageConsole console = getOrCreateConsole("Tinox");
        console.clearConsole();
        IConsoleManager mgr = ConsolePlugin.getDefault().getConsoleManager();
        mgr.addConsoles(new IConsole[]{ console });
        mgr.showConsoleView(console);

        MessageConsoleStream stream = console.newMessageStream();

        if (file == null) {
            writeLine(stream, "No Tinox file selected.");
            closeStream(stream);
            return null;
        }

        String filePath = file.getLocation().toOSString();
        String tinoxBinary = getTinoxBinary();
        writeLine(stream, "Running: " + tinoxBinary + " run " + filePath);

        try {
            Process process = new ProcessBuilder(tinoxBinary, "run", filePath)
                    .redirectErrorStream(true)
                    .start();

            new Thread(() -> {
                try (InputStream in = process.getInputStream()) {
                    byte[] buf = new byte[4096];
                    int n;
                    while ((n = in.read(buf)) != -1) {
                        stream.write(buf, 0, n);
                        stream.flush();
                    }
                    int exit = process.waitFor();
                    writeLine(stream, "\nProcess exited with code " + exit);
                } catch (Exception e) {
                    writeLine(stream, "Error reading output: " + e.getMessage());
                } finally {
                    closeStream(stream);
                }
            }).start();
        } catch (Exception e) {
            writeLine(stream, "Failed to start tinox: " + e.getMessage());
            writeLine(stream, "Binary path: " + tinoxBinary);
            closeStream(stream);
        }

        return null;
    }

    private IFile getSelectedFile(ExecutionEvent event) {
        IEditorInput input = HandlerUtil.getActiveEditorInput(event);
        if (input instanceof IFileEditorInput fei) {
            return fei.getFile();
        }
        ISelection selection = HandlerUtil.getCurrentSelection(event);
        if (selection instanceof IStructuredSelection sel) {
            Object first = sel.getFirstElement();
            if (first instanceof IFile f) return f;
        }
        return null;
    }

    private String getTinoxBinary() {
        String lspPath = Activator.getDefault().getPreferenceStore()
                .getString(TinoxLanguageServer.PREF_BINARY_PATH);
        return lspPath.replace("tinox-lsp", "tinox");
    }

    private MessageConsole getOrCreateConsole(String name) {
        IConsoleManager mgr = ConsolePlugin.getDefault().getConsoleManager();
        for (IConsole c : mgr.getConsoles()) {
            if (name.equals(c.getName()) && c instanceof MessageConsole mc) return mc;
        }
        return new MessageConsole(name, null);
    }

    private void writeLine(MessageConsoleStream stream, String msg) {
        try { stream.println(msg); stream.flush(); } catch (Exception e) { /* ignore */ }
    }

    private void closeStream(MessageConsoleStream stream) {
        try { stream.close(); } catch (Exception e) { /* ignore */ }
    }
}
