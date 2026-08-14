package tinox.eclipse;

import org.eclipse.core.resources.IProject;
import org.eclipse.core.resources.IProjectNature;
import org.eclipse.core.runtime.CoreException;

/**
 * Marks a project as a Tinox project -- no build/classpath behavior of
 * its own (Tinox isn't Java, JDT's IClasspathEntry model doesn't apply
 * here), it exists purely so TinoxSourceFolderDecorator can gate itself
 * on "is this resource inside a Tinox project" via
 * IResource.getProject().hasNature(ID), and as the general marker for
 * any future nature-scoped behavior.
 */
public class TinoxProjectNature implements IProjectNature {

    public static final String ID = "tinox.eclipse.tinoxNature";

    private IProject project;

    @Override
    public void configure() throws CoreException {
        // Nothing to configure -- see class doc comment.
    }

    @Override
    public void deconfigure() throws CoreException {
        // Nothing to deconfigure -- see class doc comment.
    }

    @Override
    public IProject getProject() {
        return project;
    }

    @Override
    public void setProject(IProject project) {
        this.project = project;
    }
}
