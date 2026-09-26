package org.omy.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.os.FileObserver
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap

@InvokeArg
class OpenExternalFileArgs {
  lateinit var path: String
  lateinit var mime: String
  var writable: Boolean = false
}

@InvokeArg
class WatchExternalFileArgs {
  lateinit var path: String
}

/**
 * WebDAV 外部编辑桥。
 *
 * FileProvider URI 由稳定文件路径生成；同一远端文件始终落在同一目录和文件名，
 * 因此跨多次打开、跨进程重启 URI 都不变。FileObserver 只落一个持久 `.dirty`
 * 标记，上传由 Rust 定时器负责，避免编辑器一次保存触发多次网络写。
 */
@TauriPlugin
class ExternalEditPlugin(private val activity: Activity) : Plugin(activity) {
  private val observers = ConcurrentHashMap<String, FileObserver>()

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

    val uri = FileProvider.getUriForFile(
      activity,
      "${activity.packageName}.fileprovider",
      file,
    )
    val action = if (args.writable) Intent.ACTION_EDIT else Intent.ACTION_VIEW
    val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or
      (if (args.writable) Intent.FLAG_GRANT_WRITE_URI_PERMISSION else 0)
    // Content-Type 可能带 charset/boundary 参数；Android Intent 过滤器只匹配基础 MIME。
    val mime = args.mime.substringBefore(';').trim().ifBlank { "application/octet-stream" }
    val intent = Intent(action).apply {
      setDataAndType(uri, mime)
      addFlags(flags)
      // 一些 Office 应用只从 ClipData 继承 URI grant，不加时会显示文件却保存失败。
      clipData = ClipData.newRawUri(file.name, uri)
    }
    if (!args.writable) {
      // 同一稳定 URI 之前可能以可编辑方式打开过；用户这次选只读时主动撤销旧写授权。
      activity.revokeUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
    } else {
      watch(file)
    }

    try {
      // Android 11+ 包可见性会让 resolveActivity() 对未声明查询的第三方应用
      // 返回 null；系统 chooser 仍能解析。直接启动并只捕获真实的无处理器错误。
      activity.startActivity(Intent.createChooser(intent, null).apply {
        addFlags(flags)
      })
    } catch (_: ActivityNotFoundException) {
      invoke.reject("没有能打开此格式的应用", "no_handler")
      return
    }
    val ret = JSObject()
    ret.put("uri", uri.toString())
    invoke.resolve(ret)
  }

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
}
