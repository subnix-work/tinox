package tinox.eclipse;

import java.io.File;

import org.eclipse.core.resources.ResourcesPlugin;
import org.eclipse.jface.wizard.WizardPage;
import org.eclipse.swt.SWT;
import org.eclipse.swt.events.ModifyEvent;
import org.eclipse.swt.events.ModifyListener;
import org.eclipse.swt.events.SelectionAdapter;
import org.eclipse.swt.events.SelectionEvent;
import org.eclipse.swt.layout.GridData;
import org.eclipse.swt.layout.GridLayout;
import org.eclipse.swt.widgets.Button;
import org.eclipse.swt.widgets.Composite;
import org.eclipse.swt.widgets.FileDialog;
import org.eclipse.swt.widgets.Label;
import org.eclipse.swt.widgets.Text;

/**
 * Single page: pick a tinox.toml, review/edit the project name, see
 * whether src/ (required) and tests/ (optional) were found next to it.
 */
public class TinoxImportWizardPage extends WizardPage {

    private Text tomlPathText;
    private Text projectNameText;
    private Label infoLabel;
    private boolean projectNameEditedByUser = false;

    protected TinoxImportWizardPage() {
        super("TinoxImportWizardPage");
        setTitle("Import Existing Tinox Project");
        setDescription("Select a tinox.toml to import its project.");
    }

    @Override
    public void createControl(Composite parent) {
        Composite container = new Composite(parent, SWT.NONE);
        container.setLayout(new GridLayout(3, false));

        Label tomlLabel = new Label(container, SWT.NONE);
        tomlLabel.setText("tinox.toml:");

        tomlPathText = new Text(container, SWT.BORDER);
        tomlPathText.setLayoutData(new GridData(SWT.FILL, SWT.CENTER, true, false));
        tomlPathText.addModifyListener(new ModifyListener() {
            @Override
            public void modifyText(ModifyEvent e) {
                onTomlPathChanged();
            }
        });

        Button browseButton = new Button(container, SWT.PUSH);
        browseButton.setText("Browse...");
        browseButton.addSelectionListener(new SelectionAdapter() {
            @Override
            public void widgetSelected(SelectionEvent e) {
                FileDialog dialog = new FileDialog(container.getShell(), SWT.OPEN);
                dialog.setFilterNames(new String[] { "tinox.toml", "All files" });
                dialog.setFilterExtensions(new String[] { "tinox.toml", "*" });
                dialog.setFileName("tinox.toml");
                String selected = dialog.open();
                if (selected != null) {
                    tomlPathText.setText(selected);
                }
            }
        });

        Label nameLabel = new Label(container, SWT.NONE);
        nameLabel.setText("Project name:");

        projectNameText = new Text(container, SWT.BORDER);
        GridData nameData = new GridData(SWT.FILL, SWT.CENTER, true, false);
        nameData.horizontalSpan = 2;
        projectNameText.setLayoutData(nameData);
        projectNameText.addModifyListener(new ModifyListener() {
            @Override
            public void modifyText(ModifyEvent e) {
                projectNameEditedByUser = true;
                validate();
            }
        });

        infoLabel = new Label(container, SWT.WRAP);
        GridData infoData = new GridData(SWT.FILL, SWT.CENTER, true, false);
        infoData.horizontalSpan = 3;
        infoLabel.setLayoutData(infoData);

        setControl(container);
        setPageComplete(false);
    }

    private void onTomlPathChanged() {
        File tomlFile = new File(tomlPathText.getText().trim());
        if (tomlFile.isFile() && tomlFile.getName().equals("tinox.toml") && !projectNameEditedByUser) {
            String name = null;
            try {
                name = TinoxToml.parsePackageName(tomlFile);
            } catch (java.io.IOException e) {
                // fall through to the directory-name fallback below
            }
            if (name == null || name.isEmpty()) {
                File parent = tomlFile.getParentFile();
                name = parent != null ? parent.getName() : "";
            }
            projectNameText.setText(name);
            projectNameEditedByUser = false; // setText() above re-triggers the listener; undo its flip
        }
        validate();
    }

    private void validate() {
        setErrorMessage(null);
        infoLabel.setText("");

        String path = tomlPathText.getText().trim();
        if (path.isEmpty()) {
            setPageComplete(false);
            return;
        }
        File tomlFile = new File(path);
        if (!tomlFile.isFile() || !tomlFile.getName().equals("tinox.toml")) {
            setErrorMessage("Select a real tinox.toml file.");
            setPageComplete(false);
            return;
        }
        File projectDir = tomlFile.getParentFile();
        File srcDir = new File(projectDir, "src");
        File testsDir = new File(projectDir, "tests");
        if (!srcDir.isDirectory()) {
            setErrorMessage("No src/ directory next to this tinox.toml -- not a valid Tinox project (src/ is required).");
            setPageComplete(false);
            return;
        }

        String name = projectNameText.getText().trim();
        if (name.isEmpty()) {
            setErrorMessage("Enter a project name.");
            setPageComplete(false);
            return;
        }
        if (ResourcesPlugin.getWorkspace().getRoot().getProject(name).exists()) {
            setErrorMessage("A project named '" + name + "' already exists in this workspace.");
            setPageComplete(false);
            return;
        }

        infoLabel.setText("src/ found (source folder)" + (testsDir.isDirectory() ? ", tests/ found (test folder)" : ", no tests/ (optional)"));
        setPageComplete(true);
    }

    File getTomlFile() {
        return new File(tomlPathText.getText().trim());
    }

    String getProjectName() {
        return projectNameText.getText().trim();
    }
}
