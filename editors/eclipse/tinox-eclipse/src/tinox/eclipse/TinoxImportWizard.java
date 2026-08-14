package tinox.eclipse;

import java.io.File;

import org.eclipse.core.resources.IProject;
import org.eclipse.core.resources.IProjectDescription;
import org.eclipse.core.resources.ResourcesPlugin;
import org.eclipse.core.runtime.CoreException;
import org.eclipse.core.runtime.IPath;
import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.core.runtime.NullProgressMonitor;
import org.eclipse.core.runtime.Path;
import org.eclipse.jface.viewers.IStructuredSelection;
import org.eclipse.jface.wizard.Wizard;
import org.eclipse.ui.IImportWizard;
import org.eclipse.ui.IWorkbench;

/**
 * File -> Import -> Tinox -> Import Existing Tinox Project. Imports an
 * EXISTING directory in place (no file copy) -- the same
 * newProjectDescription/setLocation/create/open sequence "Import
 * Existing Projects into Workspace" uses structurally, plus applying
 * TinoxProjectNature so TinoxSourceFolderDecorator picks the project up
 * immediately.
 */
public class TinoxImportWizard extends Wizard implements IImportWizard {

    private TinoxImportWizardPage page;

    @Override
    public void init(IWorkbench workbench, IStructuredSelection selection) {
        setWindowTitle("Import Existing Tinox Project");
    }

    @Override
    public void addPages() {
        page = new TinoxImportWizardPage();
        addPage(page);
    }

    @Override
    public boolean performFinish() {
        File tomlFile = page.getTomlFile();
        File projectDir = tomlFile.getParentFile();
        String projectName = page.getProjectName();

        try {
            ResourcesPlugin.getWorkspace().run(new org.eclipse.core.resources.IWorkspaceRunnable() {
                @Override
                public void run(IProgressMonitor monitor) throws CoreException {
                    IProject project = ResourcesPlugin.getWorkspace().getRoot().getProject(projectName);
                    IProjectDescription description =
                        ResourcesPlugin.getWorkspace().newProjectDescription(projectName);
                    IPath location = new Path(projectDir.getAbsolutePath());
                    // Only set an explicit location when it differs from the
                    // workspace default -- IProjectDescription.setLocation
                    // documents passing null for "use the default area",
                    // and a project already physically inside the workspace
                    // root should stay there rather than being redundantly
                    // pointed at itself.
                    IPath workspaceLocation = ResourcesPlugin.getWorkspace().getRoot().getLocation();
                    if (!location.equals(workspaceLocation.append(projectName))) {
                        description.setLocation(location);
                    }
                    description.setNatureIds(new String[] { TinoxProjectNature.ID });
                    project.create(description, monitor);
                    project.open(monitor);
                }
            }, new NullProgressMonitor());
        } catch (CoreException e) {
            page.setErrorMessage(e.getMessage());
            return false;
        }

        return true;
    }
}
