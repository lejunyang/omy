package org.omy.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ComponentName
import android.content.Intent
import android.content.pm.ResolveInfo
import android.os.FileObserver
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap

@InvokeArg
class OpenExternalFileArgs {
  lateinit var path: String
  lateinit var mime: String
  var writable: Boolean = false
  var explicitGrant: Boolean = false
}

@InvokeArg
class WatchExternalFileArgs {
  lateinit var path: String
}

@InvokeArg
class OpenLocalFileArgs {
  lateinit var path: String
  lateinit var mime: String
  lateinit var extension: String
  lateinit var appearance: String
  var chooseApplication: Boolean = false
}

@InvokeArg
class FileAssociationArgs {
  lateinit var extension: String
}

@InvokeArg
class RevokeFilesArgs {
  lateinit var paths: Array<String>
}

/**
 * Android 外部应用桥。
 *
 * WebDAV 外部编辑仍只允许应用私有缓存目录；普通本地文件由 Rust 的 token
 * 登记表解析出真实路径后才进入这里，前端不能直接传任意路径。FileProvider
 * 虽覆盖共享存储路径，但每次只向最终选中的组件授权。普通文件使用包级
 * 长期授权；加密文件的明文工作副本只使用随接收 Activity 生命周期存在的临时授权。
 */
@TauriPlugin
class ExternalEditPlugin(private val activity: Activity) : Plugin(activity) {
  private val observers = ConcurrentHashMap<String, FileObserver>()
  private val associations by lazy {
    activity.getSharedPreferences("omy-file-associations", Activity.MODE_PRIVATE)
  }
  @Volatile private var chooserVisible = false

  @Command
  fun editRoot(invoke: Invoke) {
    val root = File(activity.filesDir, "external-edit")
    if (!root.exists() && !root.mkdirs()) {
      invoke.reject("无法创建外部编辑目录", "mkdir_failed")
      return
    }
    val ret = JSObject()
    ret.put("path", root.absolutePath)
    invoke.resolve(ret)
  }

  @Command
  fun watchFile(invoke: Invoke) {
    val args = invoke.parseArgs(WatchExternalFileArgs::class.java)
    val file = File(args.path).canonicalFile
    val root = File(activity.filesDir, "external-edit/files").canonicalFile
    val rootPrefix = root.path + File.separator
    if (!file.isFile || !file.path.startsWith(rootPrefix)) {
      invoke.reject("文件不在外部编辑共享目录", "invalid_path")
      return
    }
    watch(file)
    invoke.resolve(JSObject())
  }

  @Command
  fun openFile(invoke: Invoke) {
    val args = invoke.parseArgs(OpenExternalFileArgs::class.java)
    val file = File(args.path).canonicalFile
    val root = File(activity.filesDir, "external-edit/files").canonicalFile
    val rootPrefix = root.path + File.separator
    if (!file.isFile || !file.path.startsWith(rootPrefix)) {
      invoke.reject("文件不在外部编辑缓存目录", "invalid_path")
      return
    }

    val uri = fileUri(file)
    val action = if (args.writable) Intent.ACTION_EDIT else Intent.ACTION_VIEW
    val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or
      (if (args.writable) Intent.FLAG_GRANT_WRITE_URI_PERMISSION else 0)
    val mime = cleanMime(args.mime)
    val intent = Intent(action).apply {
      setDataAndType(uri, mime)
      addFlags(flags)
      clipData = ClipData.newRawUri(file.name, uri)
    }
    if (!args.writable) {
      activity.revokeUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
    } else {
      watch(file)
    }

    val handlers = queryHandlers(intent)
    if (handlers.isEmpty()) {
      invoke.reject("没有能打开此格式的应用", "no_handler")
      return
    }
    // 应用内 chooser 让我们拿到确切包名：普通文件再显式 grant，使 Omy
    // 进程退出后该包仍能重开稳定 URI；加密文件只保留 Intent 临时授权，
    // 避免锁定后第三方还能重新读取明文工作副本。
    showApplicationChooser(
      invoke = invoke,
      base = intent,
      uri = uri,
      handlers = handlers,
      fileName = file.name,
      extension = normalizeExtension(file.extension),
      mime = mime,
      appearance = "auto",
      grantFlags = flags,
      explicitGrant = args.explicitGrant,
      allowRemember = false,
      replaceExistingGrant = true,
    )
  }

  /** 锁定时撤销全部加密工作副本的 URI 权限，并停止对应文件观察器。 */
  @Command
  fun revokeFiles(invoke: Invoke) {
    val args = invoke.parseArgs(RevokeFilesArgs::class.java)
    val root = File(activity.filesDir, "external-edit/files").canonicalFile
    val rootPrefix = root.path + File.separator
    val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
    args.paths.forEach { path ->
      val file = File(path).canonicalFile
      if (file.path.startsWith(rootPrefix)) {
        activity.revokeUriPermission(fileUri(file), flags)
        observers.remove(file.absolutePath)?.stopWatching()
      }
    }
    invoke.resolve(JSObject())
  }

  /**
   * 打开普通本地文件。默认先尝试已保存的扩展名关联；显式“用其他应用打开”
   * 则总是弹原生应用列表。选择器本身持有 invoke，用户选择或取消后才返回。
   */
  @Command
  fun openLocalFile(invoke: Invoke) {
    val args = invoke.parseArgs(OpenLocalFileArgs::class.java)
    val file = File(args.path).canonicalFile
    if (!file.isFile) {
      invoke.reject("文件不存在", "invalid_path")
      return
    }
    val extension = normalizeExtension(args.extension)
    val mime = bestMime(args.mime, extension)
    val uri = fileUri(file)
    val base = viewIntent(file, uri, mime)
    val handlers = queryHandlers(base)
    if (handlers.isEmpty()) {
      invoke.reject("没有能打开此格式的应用", "no_handler")
      return
    }

    if (!args.chooseApplication) {
      val saved = savedComponent(extension)
      val target = saved?.let { component ->
        handlers.firstOrNull { resolveComponent(it) == component }
      }
      if (target != null) {
        try {
          launchResolved(base, uri, target, extension, mime, remember = false)
          invoke.resolve(openResult(opened = true, cancelled = false, remembered = true))
          return
        } catch (_: ActivityNotFoundException) {
          clearAssociation(extension)
        } catch (_: SecurityException) {
          clearAssociation(extension)
        }
      } else if (saved != null) {
        clearAssociation(extension)
      }
    }

    showApplicationChooser(invoke, base, uri, handlers, file.name, extension, mime, args.appearance)
  }

  /** 设置页读取由 omy 自己保存的扩展名关联。 */
  @Command
  fun listFileAssociations(invoke: Invoke) {
    val items = JSArray()
    associations.all.keys
      .asSequence()
      .filter { it.startsWith("component.") }
      .map { it.removePrefix("component.") }
      .sorted()
      .forEach { extension ->
        val component = savedComponent(extension) ?: return@forEach
        val mime = associations.getString("mime.$extension", null)
        if (!associationStillValid(component, mime)) {
          clearAssociation(extension)
          return@forEach
        }
        val item = JSObject()
        item.put("extension", extensionDisplay(extension))
        item.put("appName", associations.getString("label.$extension", component.packageName))
        item.put("packageName", component.packageName)
        items.put(item)
      }
    val result = JSObject()
    result.put("supported", true)
    result.put("items", items)
    invoke.resolve(result)
  }

  /** 设置页删除一条扩展名关联，下次打开会重新弹选择器。 */
  @Command
  fun clearFileAssociation(invoke: Invoke) {
    val args = invoke.parseArgs(FileAssociationArgs::class.java)
    clearAssociation(normalizeExtension(args.extension))
    invoke.resolve(JSObject())
  }

  private fun showApplicationChooser(
    invoke: Invoke,
    base: Intent,
    uri: android.net.Uri,
    handlers: List<ResolveInfo>,
    fileName: String,
    extension: String,
    mime: String,
    appearance: String,
    grantFlags: Int = Intent.FLAG_GRANT_READ_URI_PERMISSION,
    explicitGrant: Boolean = true,
    allowRemember: Boolean = true,
    replaceExistingGrant: Boolean = false,
  ) {
    synchronized(this) {
      if (chooserVisible) {
        invoke.reject("已有应用选择器正在显示", "chooser_busy")
        return
      }
      chooserVisible = true
    }

    activity.runOnUiThread {
      OpenWithDialog(
        activity = activity,
        handlers = handlers,
        fileName = fileName,
        extension = extension,
        appearance = appearance,
        remembered = if (allowRemember) savedComponent(extension) else null,
        allowRemember = allowRemember,
        onOpen = { target, remember ->
          try {
            launchResolved(
              base,
              uri,
              target,
              extension,
              mime,
              remember && allowRemember,
              grantFlags,
              explicitGrant,
              replaceExistingGrant,
            )
            chooserVisible = false
            invoke.resolve(
              openResult(
                opened = true,
                cancelled = false,
                remembered = remember && allowRemember,
              ),
            )
          } catch (_: ActivityNotFoundException) {
            if (allowRemember) clearAssociation(extension)
            chooserVisible = false
            invoke.reject("选择的应用无法打开此格式", "no_handler")
          } catch (_: SecurityException) {
            if (allowRemember) clearAssociation(extension)
            chooserVisible = false
            invoke.reject("无法向选择的应用授予文件访问权限", "open_failed")
          }
        },
        onCancel = {
          chooserVisible = false
          invoke.resolve(openResult(opened = false, cancelled = true, remembered = false))
        },
      ).show()
    }
  }
  private fun launchResolved(
    base: Intent,
    uri: android.net.Uri,
    target: ResolveInfo,
    extension: String,
    mime: String,
    remember: Boolean,
    grantFlags: Int = Intent.FLAG_GRANT_READ_URI_PERMISSION,
    explicitGrant: Boolean = true,
    replaceExistingGrant: Boolean = false,
  ) {
    val component = resolveComponent(target)
    if (replaceExistingGrant) {
      // 同一稳定 URI 同时只交给最后选中的应用；否则普通文件每换一次
      // 编辑器都会多留一个长期授权，加密文件还可能残留上次打开的授权。
      activity.revokeUriPermission(
        uri,
        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
      )
    }
    if (explicitGrant) {
      activity.grantUriPermission(component.packageName, uri, grantFlags)
    }
    try {
      activity.startActivity(Intent(base).apply { setComponent(component) })
    } catch (error: RuntimeException) {
      // 启动失败不能遗留一个用户从未真正选用的长期授权。
      if (explicitGrant) activity.revokeUriPermission(uri, grantFlags)
      throw error
    }
    if (remember) {
      associations.edit()
        .putString("component.$extension", component.flattenToString())
        .putString("label.$extension", target.loadLabel(activity.packageManager).toString())
        .putString("mime.$extension", mime)
        .apply()
    }
  }

  private fun viewIntent(file: File, uri: android.net.Uri, mime: String): Intent =
    Intent(Intent.ACTION_VIEW).apply {
      setDataAndType(uri, mime)
      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      clipData = ClipData.newRawUri(file.name, uri)
    }

  @Suppress("DEPRECATION")
  private fun queryHandlers(intent: Intent): List<ResolveInfo> =
    activity.packageManager.queryIntentActivities(intent, android.content.pm.PackageManager.MATCH_DEFAULT_ONLY)
      .filter { it.activityInfo.packageName != activity.packageName }
      .distinctBy { resolveComponent(it).flattenToString() }
      .sortedBy { it.loadLabel(activity.packageManager).toString().lowercase(Locale.getDefault()) }

  private fun resolveComponent(info: ResolveInfo): ComponentName =
    ComponentName(info.activityInfo.packageName, info.activityInfo.name)

  @Suppress("DEPRECATION")
  private fun associationStillValid(component: ComponentName, mime: String?): Boolean {
    if (mime.isNullOrBlank()) return false
    val intent = Intent(Intent.ACTION_VIEW).apply {
      type = mime
      setComponent(component)
    }
    return activity.packageManager.queryIntentActivities(
      intent,
      android.content.pm.PackageManager.MATCH_DEFAULT_ONLY,
    ).any { resolveComponent(it) == component }
  }

  private fun savedComponent(extension: String): ComponentName? =
    associations.getString("component.$extension", null)?.let(ComponentName::unflattenFromString)

  private fun clearAssociation(extension: String) {
    associations.edit()
      .remove("component.$extension")
      .remove("label.$extension")
      .remove("mime.$extension")
      .apply()
  }

  private fun openResult(opened: Boolean, cancelled: Boolean, remembered: Boolean): JSObject =
    JSObject().apply {
      put("opened", opened)
      put("cancelled", cancelled)
      put("remembered", remembered)
    }

  private fun fileUri(file: File): android.net.Uri = FileProvider.getUriForFile(
    activity,
    "${activity.packageName}.fileprovider",
    file,
  )

  private fun cleanMime(value: String): String =
    value.substringBefore(';').trim().ifBlank { "application/octet-stream" }

  private fun bestMime(value: String, extension: String): String {
    val supplied = cleanMime(value)
    if (supplied != "application/octet-stream") return supplied
    if (extension == NO_EXTENSION) return supplied
    return android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension) ?: supplied
  }

  private fun normalizeExtension(value: String): String =
    value.trim().trimStart('.').lowercase(Locale.ROOT).ifBlank { NO_EXTENSION }

  private fun extensionDisplay(value: String): String =
    if (normalizeExtension(value) == NO_EXTENSION) "" else normalizeExtension(value)

  private fun watch(file: File) {
    val key = file.absolutePath
    if (observers.containsKey(key)) return
    val mask = FileObserver.CLOSE_WRITE or FileObserver.MODIFY or
      FileObserver.MOVED_TO or FileObserver.ATTRIB
    val dir = file.parentFile ?: return
    @Suppress("DEPRECATION")
    val observer = object : FileObserver(dir.absolutePath, mask) {
      override fun onEvent(event: Int, path: String?) {
        if (path != file.name) return
        try {
          File("${file.absolutePath}.dirty").writeText("1")
        } catch (_: IOException) {
          // Rust 回前台时还会比较内容指纹；标记失败不会漏掉修改。
        }
      }
    }
    observer.startWatching()
    observers[key] = observer
  }

  companion object {
    private const val NO_EXTENSION = "_no_extension"
  }
}
