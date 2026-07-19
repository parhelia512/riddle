package org.riddlelang.intellij

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.lang.Language
import com.intellij.openapi.fileTypes.LanguageFileType
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.platform.lsp.api.LspIntegrationProvider
import com.intellij.platform.lsp.api.ProjectWideLspClientDescriptor
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

class RiddleLspIntegrationProvider : LspIntegrationProvider {
    override fun fileOpened(
        project: Project,
        file: VirtualFile,
        clientStarter: LspIntegrationProvider.LspClientStarter,
    ) {
        if (file.fileType === RiddleFileType) {
            clientStarter.ensureClientStarted(RiddleLspClientDescriptor(project))
        }
    }
}

private class RiddleLspClientDescriptor(project: Project) :
    ProjectWideLspClientDescriptor(project, "Riddle") {
    override fun isSupportedFile(file: VirtualFile) = file.fileType === RiddleFileType
    override fun createCommandLine() = GeneralCommandLine("riddle-lsp")
}
