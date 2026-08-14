package tinox.eclipse;

import org.eclipse.jface.preference.FieldEditorPreferencePage;
import org.eclipse.jface.preference.FileFieldEditor;
import org.eclipse.ui.IWorkbench;
import org.eclipse.ui.IWorkbenchPreferencePage;

public class TinoxPreferencePage extends FieldEditorPreferencePage
        implements IWorkbenchPreferencePage {

    public TinoxPreferencePage() {
        super(GRID);
        setDescription("Configure the Tinox Language Server.");
    }

    @Override
    public void init(IWorkbench workbench) {
        setPreferenceStore(Activator.getDefault().getPreferenceStore());
    }

    @Override
    protected void createFieldEditors() {
        addField(new FileFieldEditor(
            TinoxLanguageServer.PREF_BINARY_PATH,
            "tinox-lsp binary:",
            getFieldEditorParent()
        ));
    }
}
