package tinox.eclipse;

import org.eclipse.core.resources.IFolder;
import org.eclipse.core.resources.IResource;
import org.eclipse.core.runtime.CoreException;
import org.eclipse.jface.resource.ImageDescriptor;
import org.eclipse.jface.viewers.IDecoration;
import org.eclipse.jface.viewers.ILightweightLabelDecorator;
import org.eclipse.jface.viewers.LabelProvider;

/**
 * Gives src/ and tests/ folders directly under a Tinox-natured project
 * a distinct icon -- IDecoration.REPLACE swaps the WHOLE icon (unlike
 * the corner-badge TOP_LEFT/TOP_RIGHT/etc. positions), the closest
 * available equivalent to JDT's own source-folder icon for a
 * non-Java-project resource, without depending on JDT's classpath model
 * at all. Registers globally (org.eclipse.ui.decorators has no
 * per-project-nature enablement test for folders), so every check here
 * must be a no-op outside real Tinox projects.
 */
public class TinoxSourceFolderDecorator extends LabelProvider implements ILightweightLabelDecorator {

    private static final String SRC_ICON = "icons/source_folder.png";
    private static final String TESTS_ICON = "icons/tests_folder.png";

    @Override
    public void decorate(Object element, IDecoration decoration) {
        if (!(element instanceof IFolder)) {
            return;
        }
        IFolder folder = (IFolder) element;
        String name = folder.getName();
        boolean isSrc = name.equals("src");
        boolean isTests = name.equals("tests");
        if (!isSrc && !isTests) {
            return;
        }
        // Must be a direct child of the project root, not any nested
        // folder that happens to be named src/tests.
        if (!folder.getParent().equals(folder.getProject())) {
            return;
        }
        if (!hasTinoxNature(folder)) {
            return;
        }
        ImageDescriptor icon = Activator.getDefault().getImageRegistry().getDescriptor(isSrc ? SRC_ICON : TESTS_ICON);
        if (icon == null) {
            icon = registerIcon(isSrc ? SRC_ICON : TESTS_ICON);
        }
        if (icon != null) {
            decoration.addOverlay(icon, IDecoration.REPLACE);
        }
    }

    private boolean hasTinoxNature(IResource resource) {
        try {
            return resource.getProject().isAccessible() && resource.getProject().hasNature(TinoxProjectNature.ID);
        } catch (CoreException e) {
            return false;
        }
    }

    private ImageDescriptor registerIcon(String path) {
        ImageDescriptor descriptor = ImageDescriptor.createFromURL(
            Activator.getDefault().getBundle().getEntry(path));
        Activator.getDefault().getImageRegistry().put(path, descriptor);
        return descriptor;
    }
}
