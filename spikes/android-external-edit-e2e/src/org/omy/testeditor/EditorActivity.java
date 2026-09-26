package org.omy.testeditor;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/**
 * omy Android 外部编辑端到端测试专用 Activity。
 *
 * ACTION_VIEW 只读回读并尝试写入（应失败）；ACTION_EDIT 写入固定内容。所有观察结果
 * 持久化在 SharedPreferences，测试脚本通过 run-as 回读，不依赖截图猜测。
 */
public final class EditorActivity extends Activity {
    private static final String EDITED = "edited-by-omy-test-editor-v1\n";

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Intent intent = getIntent();
        Uri uri = intent.getData();
        String action = intent.getAction();
        boolean hasRead = (intent.getFlags() & Intent.FLAG_GRANT_READ_URI_PERMISSION) != 0;
        boolean hasWrite = (intent.getFlags() & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) != 0;
        String before = "";
        String readError = "";
        String writeResult = "not-attempted";
        try {
            if (uri != null) {
                try (InputStream in = getContentResolver().openInputStream(uri)) {
                    before = in == null ? "" : new String(in.readAllBytes(), StandardCharsets.UTF_8);
                }
            }
        } catch (Exception e) {
            readError = e.getClass().getName() + ":" + String.valueOf(e.getMessage());
        }

        if (uri != null) {
            try (OutputStream out = getContentResolver().openOutputStream(uri, "wt")) {
                if (out == null) throw new IllegalStateException("null output stream");
                out.write(EDITED.getBytes(StandardCharsets.UTF_8));
                out.flush();
                writeResult = "ok";
            } catch (Exception e) {
                writeResult = e.getClass().getName() + ":" + String.valueOf(e.getMessage());
            }
        }

        getSharedPreferences("results", MODE_PRIVATE).edit()
            .putString("action", action == null ? "" : action)
            .putString("uri", uri == null ? "" : uri.toString())
            .putBoolean("flag_read", hasRead)
            .putBoolean("flag_write", hasWrite)
            .putString("before", before)
            .putString("read_error", readError)
            .putString("write_result", writeResult)
            .putLong("timestamp", System.currentTimeMillis())
            .apply();
        finish();
    }
}
