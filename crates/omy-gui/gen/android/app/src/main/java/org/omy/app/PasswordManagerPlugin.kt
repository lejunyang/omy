package org.omy.app

import android.app.Activity
import android.os.Build
import android.os.CancellationSignal
import androidx.core.content.ContextCompat
import androidx.credentials.CreatePasswordRequest
import androidx.credentials.CreateCredentialResponse
import androidx.credentials.CredentialManager
import androidx.credentials.CredentialManagerCallback
import androidx.credentials.GetCredentialRequest
import androidx.credentials.GetCredentialResponse
import androidx.credentials.GetPasswordOption
import androidx.credentials.PasswordCredential
import androidx.credentials.exceptions.CreateCredentialCancellationException
import androidx.credentials.exceptions.CreateCredentialException
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import androidx.credentials.exceptions.NoCredentialException
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class SavePasswordManagerCredentialArgs {
  lateinit var label: String
  lateinit var secret: String
}

/** Android Credential Manager 桥。
 *
 * 这里只搬运系统选择器返回的一条 PasswordCredential。Rust 侧收到后立即走 KDF，
 * 不把 secret 转发给 WebView。第三方 provider 需要 Android 14+；较旧系统继续用
 * 已启用的 WebView Autofill，不能在这里假装支持显式选择。
 *
 * 标准 CreatePasswordRequest 只允许普通应用给出 id/password。条目的调用方来源
 * 由系统写成包名和签名；自定义 Web origin 只开放给持有系统特权权限的浏览器。
 * 因而这里不能替 KeePassDX 同时写入 KeePassXC 查询所需的 URL，跨端限制必须
 * 如实留在 UI 和文档中，不能伪造 origin 绕过系统的防钓鱼边界。
 */
@TauriPlugin
class PasswordManagerPlugin(private val activity: Activity) : Plugin(activity) {
  private val manager by lazy { CredentialManager.create(activity) }
  private val executor by lazy { ContextCompat.getMainExecutor(activity) }

  @Command
  fun status(invoke: Invoke) {
    val result = JSObject()
    val available = Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE
    result.put("available", available)
    result.put("reason", if (available) "" else "requires_android_14")
    invoke.resolve(result)
  }

  @Command
  fun selectPassword(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
      invoke.reject("Android 14 or newer is required", "password_manager_unsupported")
      return
    }
    val request = GetCredentialRequest.Builder()
      .addCredentialOption(GetPasswordOption())
      .build()
    manager.getCredentialAsync(
      context = activity,
      request = request,
      cancellationSignal = CancellationSignal(),
      executor = executor,
      callback = object : CredentialManagerCallback<GetCredentialResponse, GetCredentialException> {
        override fun onResult(result: GetCredentialResponse) {
          val credential = result.credential
          if (credential !is PasswordCredential) {
            invoke.reject("Selected credential is not a password", "password_manager_unsupported")
            return
          }
          val response = JSObject()
          response.put("id", credential.id)
          response.put("secret", credential.password)
          invoke.resolve(response)
        }

        override fun onError(e: GetCredentialException) {
          when (e) {
            is GetCredentialCancellationException ->
              invoke.reject("Credential selection cancelled", "user_cancelled")
            is NoCredentialException ->
              invoke.reject("No matching credential", "password_manager_not_found")
            else -> invoke.reject("Credential selection failed", "password_manager_failed")
          }
        }
      }
    )
  }

  @Command
  fun savePassword(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
      invoke.reject("Android 14 or newer is required", "password_manager_unsupported")
      return
    }
    val args = invoke.parseArgs(SavePasswordManagerCredentialArgs::class.java)
    // KeePassDX 会从系统提供的 CallingAppInfo 写入 org.omy.app 与签名指纹；
    // 这里故意不设置 origin。普通应用没有 CREDENTIAL_MANAGER_SET_ORIGIN，
    // 强行冒充 https://credentials.omy.app 会被系统拒绝。
    val request = CreatePasswordRequest(id = args.label, password = args.secret)
    manager.createCredentialAsync(
      context = activity,
      request = request,
      cancellationSignal = CancellationSignal(),
      executor = executor,
      callback = object : CredentialManagerCallback<CreateCredentialResponse, CreateCredentialException> {
        override fun onResult(result: CreateCredentialResponse) {
          // CreateCredentialResponse 本身没有条目 id；label 是之后选择时返回的
          // PasswordCredential.id，也是跨 provider 最稳定的公开标识。
          val response = JSObject()
          response.put("id", args.label)
          invoke.resolve(response)
        }

        override fun onError(e: CreateCredentialException) {
          if (e is CreateCredentialCancellationException) {
            invoke.reject("Credential creation cancelled", "user_cancelled")
          } else {
            invoke.reject("Credential creation failed", "password_manager_failed")
          }
        }
      }
    )
  }
}
