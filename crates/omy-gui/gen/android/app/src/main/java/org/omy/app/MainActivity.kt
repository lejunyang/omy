package org.omy.app

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // 内容延伸到状态栏与导航栏下面。代价是顶栏会被状态栏压住、
    // 底部导航会被手势条盖住，靠 CSS 的 env(safe-area-inset-*)
    // 把内容顶回来——两者必须一起看，只改一边就会出现
    // 「界面看着正常但点不到」
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // debug 构建才开远程调试：这个开关一开，任何能连上 adb 的程序
    // 都能接管 WebView，而里面是解密后的明文内容。
    // 与桌面端 OMY_GUI_CDP_PORT 默认关闭是同一个理由。
    if (BuildConfig.DEBUG) {
      WebView.setWebContentsDebuggingEnabled(true)
    }
  }
}
