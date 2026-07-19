# Riddle for JetBrains IDEs

This Kotlin plugin registers `.rid` files and starts `riddle-lsp` through the IntelliJ LSP integration API.

It targets IntelliJ Platform 2026.2 or newer. IntelliJ IDEA editions are supported; Android Studio is not a supported target.

Use JDK 21 or newer for the Gradle JVM, make sure `riddle-lsp` is available in the IDE process `PATH`, then build the plugin:

```powershell
.\gradlew.bat buildPlugin
```

The installable ZIP is written to `build/distributions`. In the IDE, open **Settings | Plugins**, choose **Install Plugin from Disk...**, select the ZIP, and restart the IDE.
