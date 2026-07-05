package com.pixie.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import androidx.activity.result.ActivityResult
import app.tauri.Logger
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@TauriPlugin
class FolderPickerPlugin(private val activity: Activity) : Plugin(activity) {

    @app.tauri.annotation.Command
    fun pickDirectory(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
            startActivityForResult(invoke, intent, "folderPickerResult")
        } catch (ex: Exception) {
            val message = ex.message ?: "Failed to open folder picker"
            Logger.error(message)
            invoke.reject(message)
        }
    }

    @ActivityCallback
    fun folderPickerResult(invoke: Invoke, result: ActivityResult) {
        try {
            when (result.resultCode) {
                Activity.RESULT_OK -> {
                    val callResult = JSObject()
                    val data: Intent? = result.data
                    if (data != null) {
                        val uri: Uri? = data.data
                        if (uri != null) {
                            callResult.put("folder", uri.toString())
                        } else {
                            callResult.put("folder", null)
                        }
                    } else {
                        callResult.put("folder", null)
                    }
                    invoke.resolve(callResult)
                }
                Activity.RESULT_CANCELED -> invoke.reject("Folder picker cancelled")
                else -> invoke.reject("Failed to pick folder")
            }
        } catch (ex: Exception) {
            val message = ex.message ?: "Failed to read folder pick result"
            Logger.error(message)
            invoke.reject(message)
        }
    }
}
