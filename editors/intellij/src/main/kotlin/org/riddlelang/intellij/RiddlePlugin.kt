package org.riddlelang.intellij

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.lang.Language
import com.intellij.openapi.fileTypes.LanguageFileType
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.platform.lsp.api.LspServerSupportProvider
import com.intellij.platform.lsp.api.ProjectWideLspServerDescriptor
import javax.swing.Icon

object RiddleLanguage : Language("Riddle") {
    override fun getDisplayName() = "Riddle"
}

object RiddleFileType : LanguageFileType(RiddleLanguage) {
    override fun getName() = "Riddle"
    override fun getDescription() = "Riddle source file"
    override fun getDefaultExtension() = "rid"
    override fun getIcon(): Icon? = null
}

class RiddleLspServerSupportProvider : LspServerSupportProvider {
    override fun fileOpened(
        project: Project,
        file: VirtualFile,
        serverStarter: LspServerSupportProvider.LspServerStarter,
    ) {
        if (file.fileType === RiddleFileType) {
            serverStarter.ensureServerStarted(RiddleLspServerDescriptor(project))
        }
    }
}

private class RiddleLspServerDescriptor(project: Project) :
    ProjectWideLspServerDescriptor(project, "Riddle") {
    override fun isSupportedFile(file: VirtualFile) = file.fileType === RiddleFileType
    override fun createCommandLine() = GeneralCommandLine("riddle-lsp")
}
