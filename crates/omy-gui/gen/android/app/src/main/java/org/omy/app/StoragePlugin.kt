package org.omy.app

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.Settings
import androidx.activity.result.ActivityResult
import androidx.core.app.ActivityCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * API 29 及以下旧存储权限的别名。
 *
 * 放在顶层而不是 companion object 里：`@TauriPlugin` 注解在类声明处求值，
 * 引用类自身的 companion 成员会形成循环，而 private 成员在注解位置也不可见。
 */
private const val ALIAS_LEGACY_STORAGE = "legacyStorage"

/**
 * 全盘存储访问权限。
 *
 * # 为什么必须放在 Kotlin 侧
 *
 * 判定要调 `Environment.isExternalStorageManager()`，申请要跳
 * `ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION` 设置页，两者都是 Java API。
 * 从 Rust 走 JNI 需要 `unsafe { JavaVM::from_raw }`，而 omy-gui 的 lint 是
 * `unsafe_code = "forbid"`——为了这点权限判定去破例，不如把这 100 行写在
 * 它本来该在的地方。
 *
 * # 为什么要这个权限而不是 SAF
 *
 * 见 AndroidManifest.xml 中 MANAGE_EXTERNAL_STORAGE 的注释：omy 的核心是
 * 路径语义的 std::fs，SAF 的 content:// URI 接不上原子写。
 */
@TauriPlugin(
  // 这里声明的别名是 requestPermissionForAliases 的唯一来源：别名没声明时
  // getPermissionStringsForAliases 返回空数组，请求会被**静默丢弃**，
  // invoke 永不 resolve，前端就一直转圈。
  //
  // 只声明 API 29 及以下的旧权限。MANAGE_EXTERNAL_STORAGE 是 special
  // permission，不能走 requestPermissions 那条路，写进来反而会让
  // checkPermissions 报出一个永远为 PROMPT 的假状态。
  permissions = [
    Permission(
      strings = [
        Manifest.permission.READ_EXTERNAL_STORAGE,
        Manifest.permission.WRITE_EXTERNAL_STORAGE,
      ],
      alias = ALIAS_LEGACY_STORAGE,
    )
  ]
)
class StoragePlugin(private val activity: Activity) : Plugin(activity) {
  /**
   * 查询当前是否已拿到全盘访问。
   *
   * 返回 `granted`（能否读写共享存储）与 `mode`（走的是哪条路径）。
   * `mode` 要单独给前端，因为两条路径的申请交互完全不同：
   * `all-files` 跳设置页且用户可能一去不回，`legacy` 是标准运行时弹窗。
   */
  @Command
  fun checkAccess(invoke: Invoke) {
    invoke.resolve(currentState())
  }

  /**
   * 列出所有存储卷的根目录路径。
   *
   * 供侧栏在拿到权限后列出「内部存储」和 SD 卡。放在 Kotlin 侧是因为
   * `StorageManager.getStorageVolumes()` 是 Java API；Rust 侧只能猜路径，
   * 而 SD 卡的挂载点形如 `/storage/A1B2-C3D4`，卷 ID 由系统随机分配，
   * 猜不出来。
   *
   * 只在 API 30+ 走 `StorageVolume.getDirectory()`——它和
   * MANAGE_EXTERNAL_STORAGE 的最低版本正好一致，所以不需要反射拿
   * 私有的 getPath()。低版本退回 `Environment` 的主存储路径。
   */
  @Command
  fun storageVolumes(invoke: Invoke) {
    val volumes = JSArray()

    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val sm = activity.getSystemService(StorageManager::class.java)
      for (v in sm.storageVolumes) {
        // getDirectory() 在卷未挂载时返回 null（例如 SD 卡已拔出）。
        // 必须跳过：否则侧栏会多出一个点进去就报错的条目。
        val dir = v.directory ?: continue
        val item = JSObject()
        item.put("path", dir.absolutePath)
        item.put("removable", v.isRemovable)
        item.put("primary", v.isPrimary)
        // 卷的显示名由系统本地化（「内部共享存储空间」/「SD 卡」）。
        // 主存储不取系统名：那串文案各厂商不一，且前端对主存储有
        // 自己的翻译键，取了反而绕过 i18n。
        item.put("label", if (v.isPrimary) null else v.getDescription(activity))
        volumes.put(item)
      }
    } else {
      @Suppress("DEPRECATION")
      val dir = Environment.getExternalStorageDirectory()
      if (dir != null) {
        val item = JSObject()
        item.put("path", dir.absolutePath)
        item.put("removable", false)
        item.put("primary", true)
        item.put("label", null)
        volumes.put(item)
      }
    }

    val ret = JSObject()
    ret.put("volumes", volumes)
    invoke.resolve(ret)
  }

  /**
   * 申请全盘访问。
   *
   * API 30+ 没有弹窗可用：`MANAGE_EXTERNAL_STORAGE` 属于 special permission，
   * 只能把用户送到设置页自己开。API 29 及以下退回普通运行时权限。
   */
  @Command
  fun requestAccess(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
      // API 29 及以下：走标准运行时权限。已授予时直接返回，
      // 否则重复申请会被系统直接判为拒绝。
      if (hasLegacyPermission()) {
        invoke.resolve(currentState())
      } else {
        requestPermissionForAliases(
          arrayOf(ALIAS_LEGACY_STORAGE),
          invoke,
          "onLegacyPermissionResult",
        )
      }
      return
    }

    if (Environment.isExternalStorageManager()) {
      invoke.resolve(currentState())
      return
    }

    // 带包名的 Intent 直达本应用的开关；不带包名只能打开总列表，
    // 用户得在几十个应用里自己找。部分定制系统不认带包名的形式，
    // 所以失败时退回不带包名的总列表，而不是直接报错。
    val withPackage = Intent(
      Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
      Uri.parse("package:${activity.packageName}"),
    )
    val intent = if (canResolve(withPackage)) {
      withPackage
    } else {
      Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
    }

    if (!canResolve(intent)) {
      // 例如被裁剪过的系统或 TV 设备上根本没有这个设置页。
      // 明确报出来，不要让前端一直转圈等一个永远不会回来的结果。
      invoke.reject("此设备没有「所有文件访问权限」设置页", "settings_unavailable")
      return
    }

    startActivityForResult(invoke, intent, "onAllFilesAccessResult")
  }

  /**
   * 设置页返回。
   *
   * **必须重新查询，不能看 result.resultCode。** 这个权限不是弹窗而是设置页，
   * 用户按返回键回来时 resultCode 恒为 RESULT_CANCELED——即使他刚刚把开关
   * 打开了。照 resultCode 判断会得到「明明授权了却仍报无权限」，
   * 而且因为流程看起来完全正常，极难定位。
   */
  @ActivityCallback
  // result 刻意不用，但**不能删**：Tauri 通过反射按
  // (Invoke, ActivityResult) 这个签名调用回调，少一个参数就找不到方法，
  // 表现为点了授权后回来毫无反应。
  @Suppress("UNUSED_PARAMETER")
  fun onAllFilesAccessResult(invoke: Invoke, result: ActivityResult) {
    invoke.resolve(currentState())
  }

  /**
   * 用系统原生的目录选择器（SAF）选一个文件夹，返回它的文件系统路径。
   *
   * # 为什么能把 SAF 的结果当路径用
   *
   * omy 的核心是路径语义的 std::fs，SAF 的 content:// URI 接不上原子写。但
   * 我们已经拿到了全盘访问（MANAGE_EXTERNAL_STORAGE），共享存储上的路径本来
   * 就能直接读写；这里只是借系统选择器的界面让用户挑位置，再把树 URI 换算
   * 回路径。原来 Android 上「打开文件夹」直接报不支持，而那是移动端首屏
   * 唯一的悬浮按钮。
   *
   * 换算不出来（第三方云盘等不落在本机卷上的提供方）时明确报错，而不是
   * 返回一个读不了的假路径。用户取消时返回 `path: null`。
   */
  @Command
  fun pickDirectory(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
    if (!canResolve(intent)) {
      invoke.reject("此设备没有系统目录选择器", "picker_unavailable")
      return
    }
    startActivityForResult(invoke, intent, "onPickDirectoryResult")
  }

  @ActivityCallback
  fun onPickDirectoryResult(invoke: Invoke, result: ActivityResult) {
    val ret = JSObject()
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null) {
      ret.put("path", null)
      invoke.resolve(ret)
      return
    }
    val path = treeUriToPath(uri)
    if (path == null) {
      invoke.reject("这个位置不在本机存储上，无法直接浏览", "unsupported_location")
      return
    }
    ret.put("path", path)
    invoke.resolve(ret)
  }

  /**
   * 把 ExternalStorage 提供方的树 URI 换算成路径。
   *
   * 文档 ID 形如 `primary:Download/相册`（主存储）或 `A1B2-C3D4:DCIM`（SD 卡，
   * 冒号前是卷 UUID）；下载提供方有时给 `raw:/storage/emulated/0/...`。
   * 其余形式（云盘、媒体库虚拟目录）不对应本机路径，返回 null。
   */
  private fun treeUriToPath(uri: Uri): String? {
    val docId = try {
      DocumentsContract.getTreeDocumentId(uri)
    } catch (e: IllegalArgumentException) {
      return null
    }
    if (docId.startsWith("raw:")) {
      return existingDir(docId.removePrefix("raw:"))
    }
    val colon = docId.indexOf(':')
    if (colon < 0) return null
    val volumeId = docId.substring(0, colon)
    val rel = docId.substring(colon + 1).trim('/')
    val root: String? = if (volumeId.equals("primary", ignoreCase = true)) {
      @Suppress("DEPRECATION")
      Environment.getExternalStorageDirectory()?.absolutePath
    } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val sm = activity.getSystemService(StorageManager::class.java)
      sm.storageVolumes.firstOrNull { it.uuid.equals(volumeId, ignoreCase = true) }
        ?.directory?.absolutePath
    } else {
      "/storage/$volumeId"
    }
    if (root == null) return null
    return existingDir(if (rel.isEmpty()) root else "$root/$rel")
  }

  private fun existingDir(path: String): String? {
    val f = java.io.File(path)
    return if (f.isDirectory) f.absolutePath else null
  }

  /** API 29 及以下的运行时权限回调。同样以实查结果为准。 */
  @PermissionCallback
  fun onLegacyPermissionResult(invoke: Invoke) {
    invoke.resolve(currentState())
  }

  /**
   * 以系统实际状态为唯一真相，不缓存——用户随时可能去设置里关掉。
   *
   * 注意 mode 表达的是「这台设备用哪种授权机制」，和当前有没有授权无关：
   * API 30+ 恒为 all-files，低版本恒为 legacy，撤销权限后也不变，变的只有
   * granted。前端据此决定要不要显示授权入口（not-applicable 时不显示），
   * 所以不能改成「未授权就报 not-applicable」——那样按钮会消失，用户再也
   * 点不回来。
   */
  private fun currentState(): JSObject {
    val granted: Boolean
    val mode: String
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      granted = Environment.isExternalStorageManager()
      mode = "all-files"
    } else {
      granted = hasLegacyPermission()
      mode = "legacy"
    }

    val ret = JSObject()
    ret.put("granted", granted)
    ret.put("mode", mode)
    return ret
  }

  private fun hasLegacyPermission(): Boolean =
    ActivityCompat.checkSelfPermission(activity, Manifest.permission.READ_EXTERNAL_STORAGE) ==
      PackageManager.PERMISSION_GRANTED

  private fun canResolve(intent: Intent): Boolean =
    intent.resolveActivity(activity.packageManager) != null
}
