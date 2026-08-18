import java.io.File
import org.apache.tools.ant.taskdefs.condition.Os
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.logging.LogLevel
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.TaskAction

open class BuildTask : DefaultTask() {
    @Input
    var rootDirRel: String? = null
    @Input
    var target: String? = null
    @Input
    var release: Boolean? = null

    @TaskAction
    fun assemble() {
        val executable = """deno""";
        try {
            runTauriCli(executable)
        } catch (e: Exception) {
            if (Os.isFamily(Os.FAMILY_WINDOWS)) {
                // Try different Windows-specific extensions
                val fallbacks = listOf(
                    "$executable.exe",
                    "$executable.cmd",
                    "$executable.bat",
                )
                
                var lastException: Exception = e
                for (fallback in fallbacks) {
                    try {
                        runTauriCli(fallback)
                        return
                    } catch (fallbackException: Exception) {
                        lastException = fallbackException
                    }
                }
                throw lastException
            } else {
                throw e;
            }
        }
    }

    fun runTauriCli(executable: String) {
        val rootDirRel = rootDirRel ?: throw GradleException("rootDirRel cannot be null")
        val target = target ?: throw GradleException("target cannot be null")
        val release = release ?: throw GradleException("release cannot be null")
        val args = listOf("task", "tauri", "android", "android-studio-script");

        val tauriDir = File(project.projectDir, rootDirRel)

        // sherpa-onnx-sys resolves its libraries from SHERPA_ONNX_LIB_DIR and
        // from nothing else, and the value is PER ABI.
        //
        // This is the only place it can be set correctly. A multi-ABI android
        // build runs one of these tasks per ABI, but they all inherit the single
        // environment of the gradle daemon, so an env var exported outside is
        // right for one ABI and wrong for the other three. Setting it here, keyed
        // on this task's own target, is what makes the four-ABI universal build
        // work rather than only the arm64 split.
        //
        // Getting it wrong is loud rather than silent (linking an arm64 `.so`
        // into an x86_64 object is a linker error naming the architecture), so
        // the bad direction is a build that stops.
        //
        // `target` is the Tauri CLI's arch name; the directory is Android's ABI
        // name, and the two disagree for three of the four.
        val abi = when (target) {
            "aarch64" -> "arm64-v8a"
            "armv7" -> "armeabi-v7a"
            "i686" -> "x86"
            "x86_64" -> "x86_64"
            else -> throw GradleException("unknown rust target for sherpa-onnx: $target")
        }
        // tauriDir is packages/loca-client/src/tauri, so the repo root is four up.
        val repoRoot = tauriDir.resolve("../../../..").canonicalFile
        val sherpaLibDir = File(repoRoot, "vendor/sherpa-onnx/android/jniLibs/$abi")
        if (!sherpaLibDir.isDirectory) {
            throw GradleException(
                "vendor/sherpa-onnx is not staged for $abi (looked in $sherpaLibDir).\n" +
                    "Run: deno run -A scripts/vendor-sherpa.ts",
            )
        }

        project.exec {
            workingDir(tauriDir)
            environment("SHERPA_ONNX_LIB_DIR", sherpaLibDir.absolutePath)
            executable(executable)
            args(args)
            if (project.logger.isEnabled(LogLevel.DEBUG)) {
                args("-vv")
            } else if (project.logger.isEnabled(LogLevel.INFO)) {
                args("-v")
            }
            if (release) {
                args("--release")
            }
            args(listOf("--target", target))
        }.assertNormalExitValue()
    }
}