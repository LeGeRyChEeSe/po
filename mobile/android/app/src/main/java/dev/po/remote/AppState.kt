package dev.po.remote

import android.app.NotificationManager
import android.content.Intent
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.app.NotificationCompat
import uniffi.po_mobile.Alert
import uniffi.po_mobile.ApprovalRec
import uniffi.po_mobile.CommandRec
import uniffi.po_mobile.CoreException
import uniffi.po_mobile.InputRec
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.LinkState
import uniffi.po_mobile.Listener
import uniffi.po_mobile.MenuRec
import uniffi.po_mobile.PcInfo
import uniffi.po_mobile.ProjectRec
import uniffi.po_mobile.QuestionRec
import uniffi.po_mobile.SessionRec
import uniffi.po_mobile.SessionUpdate
import uniffi.po_mobile.SignIn
import uniffi.po_mobile.StatusRec

/** Where the user is. */
sealed class Screen {
    data object SignIn : Screen()
    data object Home : Screen()
    data class Project(val pc: String, val path: String, val name: String) : Screen()
    data class Conversation(val pc: String, val key: String) : Screen()
    data object Settings : Screen()
}

/**
 * Everything the screens show, kept as Compose state, and the actions on the core. The core's
 * calls block: they run on the app's threads, and their results come back on the main one.
 */
class AppState(private val app: PoApp) : Listener {
    private val main = Handler(Looper.getMainLooper())

    var account by mutableStateOf<String?>(null)
    var signIn by mutableStateOf<SignIn?>(null)
    var busyText by mutableStateOf<String?>(null)
    var error by mutableStateOf<String?>(null)
    var toast by mutableStateOf<String?>(null)

    var pcs by mutableStateOf<List<PcInfo>>(emptyList())
    var ready by mutableStateOf(true)
    var refreshing by mutableStateOf(false)
    val links = mutableStateMapOf<String, LinkState>()
    val projects = mutableStateMapOf<String, List<ProjectRec>>()
    var sessions by mutableStateOf<List<SessionRec>>(emptyList())

    // The conversation shown.
    var open by mutableStateOf<Pair<String, String>?>(null)
    val rows = mutableStateListOf<ItemRec>()
    var status by mutableStateOf<StatusRec?>(null)
    var running by mutableStateOf(false)
    var approval by mutableStateOf<ApprovalRec?>(null)
    var question by mutableStateOf<QuestionRec?>(null)
    var closed by mutableStateOf(false)
    var commands by mutableStateOf<List<CommandRec>>(emptyList())
    var menu by mutableStateOf<MenuRec?>(null)
    var draft by mutableStateOf("")

    val stack = mutableStateListOf<Screen>(Screen.Home)
    val screen: Screen get() = stack.last()
    var inForeground = true

    private fun ui(block: () -> Unit) {
        main.post(block)
    }

    /** Runs a call of the core off the main thread; its result (or its error) comes back on it. */
    private fun <T> work(label: String?, call: () -> T, done: (T) -> Unit = {}) {
        if (label != null) busyText = label
        app.io.execute {
            val result = runCatching(call)
            ui {
                if (label != null && busyText == label) busyText = null
                result.onSuccess(done).onFailure {
                    android.util.Log.w("Po", "call failed", it)
                    error = describe(it)
                    // A sign-in that failed gives its button back.
                    signIn = null
                }
            }
        }
    }

    private fun describe(e: Throwable): String = when (e) {
        is CoreException.Network -> app.getString(R.string.error_network, e.reason)
        is CoreException.Unreachable -> app.getString(R.string.error_unreachable, e.reason)
        is CoreException.Refused -> app.getString(R.string.error_refused)
        is CoreException.Pc -> e.reason
        is CoreException.Denied -> app.getString(R.string.error_denied)
        is CoreException.Expired -> app.getString(R.string.error_expired)
        is CoreException.SignedOut -> app.getString(R.string.error_signed_out)
        else -> e.message ?: e.toString()
    }

    fun go(screen: Screen) {
        stack.add(screen)
    }

    fun back(): Boolean {
        if (stack.size <= 1) return false
        val left = stack.removeAt(stack.size - 1)
        if (left is Screen.Conversation) closeConversation()
        return true
    }

    fun start() {
        work(app.getString(R.string.busy_starting), { app.core.resume() }) { login ->
            account = login
            if (login == null) {
                stack.clear(); stack.add(Screen.SignIn)
            } else {
                refresh()
            }
        }
    }

    // Signing in: the same GitHub account as in Po on the PC is the pairing.
    fun beginSignIn() {
        work(app.getString(R.string.busy_github), { app.core.signInStart() }) { code ->
            signIn = code
            work(null, { app.core.signInWait() }) { login ->
                signIn = null
                account = login
                stack.clear(); stack.add(Screen.Home)
                refresh()
            }
        }
    }

    fun signOut() {
        work(app.getString(R.string.busy_signing_out), { app.core.signOut() }) {
            account = null
            pcs = emptyList(); projects.clear(); links.clear(); closeConversation()
            stack.clear(); stack.add(Screen.SignIn)
            app.stopService(Intent(app, LinkService::class.java))
        }
    }

    fun refresh() {
        refreshing = true
        app.io.execute {
            val result = runCatching { app.core.pcs() }
            ui {
                refreshing = false
                result.onSuccess { list ->
                    pcs = list.pcs; ready = list.ready
                    // Each PC that can be reached: its projects.
                    list.pcs.filter { it.reachable }.forEach { loadProjects(it.id) }
                }.onFailure { error = describe(it) }
            }
        }
    }

    fun loadProjects(pc: String) {
        app.io.execute {
            val result = runCatching { app.core.projects(pc) }
            ui { result.onSuccess { projects[pc] = it } }
        }
    }

    fun loadSessions(pc: String, path: String) {
        work(null, { app.core.sessions(pc, path) }) { sessions = it }
    }

    fun openConversation(pc: String, key: String) {
        closeConversation()
        open = pc to key
        rows.clear(); status = null; running = false; approval = null; question = null; closed = false; menu = null
        go(Screen.Conversation(pc, key))
        app.io.execute { app.core.open(pc, key) }
        work(null, { app.core.commands(pc, key) }) { commands = it }
        // Kept connected while the app is in the background, to hear the end of a turn.
        app.startForegroundService(Intent(app, LinkService::class.java))
    }

    fun closeConversation() {
        val (pc, key) = open ?: return
        open = null
        app.io.execute { app.core.closeView(pc, key) }
    }

    fun newSession(pc: String, project: String, prompt: String?) {
        work(app.getString(R.string.busy_opening), { app.core.newSession(pc, project, prompt?.takeIf { it.isNotBlank() }) }) { key ->
            openConversation(pc, key)
        }
    }

    fun openSaved(pc: String, session: SessionRec) {
        if (session.live) {
            openConversation(pc, session.key)
            return
        }
        // A saved session: read as it was, until the user writes in it (then it opens again).
        openConversation(pc, session.key)
    }

    fun resumeSaved(pc: String, saved: String) {
        work(app.getString(R.string.busy_opening), { app.core.resumeSession(pc, saved) }) { key ->
            if (stack.lastOrNull() is Screen.Conversation) stack.removeAt(stack.size - 1)
            openConversation(pc, key)
        }
    }

    fun send(input: InputRec, after: (String) -> Unit = {}) {
        val (pc, key) = open ?: return
        work(null, { app.core.send(pc, key, input) }, after)
    }

    fun submit() {
        val text = draft.trim()
        if (text.isEmpty()) return
        val (pc, key) = open ?: return
        // A saved session opens again (without a terminal) for the message to go in.
        if (key.startsWith("saved:")) {
            val saved = key.removePrefix("saved:")
            work(app.getString(R.string.busy_opening), { app.core.resumeSession(pc, saved) }) { live ->
                if (stack.lastOrNull() is Screen.Conversation) stack.removeAt(stack.size - 1)
                openConversation(pc, live)
                send(if (text.startsWith("/")) InputRec.Command(text) else InputRec.Prompt(text))
            }
            draft = ""
            return
        }
        send(if (text.startsWith("/")) InputRec.Command(text) else InputRec.Prompt(text)) { answer ->
            if (text.startsWith("/") && answer.isNotBlank()) toast = answer.lines().first()
        }
        draft = ""
    }

    /** A command chosen in the commands list or in a menu: its menu first, if it needs a choice. */
    fun command(line: String) {
        val (pc, key) = open ?: return
        work(null, { app.core.menu(pc, key, line) }) { found ->
            if (found != null && found.items.isNotEmpty()) menu = found else send(InputRec.Command(line))
        }
    }

    fun chooseMenuItem(action: String, command: String) {
        menu = null
        when (action) {
            "fill" -> draft = command
            "show" -> send(InputRec.Command(command))
            else -> command(command)
        }
    }

    fun closeSession() {
        val (pc, key) = open ?: return
        work(null, { app.core.closeSession(pc, key) }) { back() }
    }

    // What the core says on its own.

    override fun onSession(pc: String, key: String, update: SessionUpdate) = ui {
        if (open != pc to key) return@ui
        // Rows before `from` are unchanged: only the end is replaced.
        while (rows.size > update.from.toInt()) rows.removeAt(rows.size - 1)
        rows.addAll(update.items)
        status = update.status
        running = update.busy
        approval = update.approval
        question = update.question
        closed = update.closed
    }

    override fun onLink(pc: String, state: LinkState) = ui { links[pc] = state }

    override fun onAlert(pc: String, key: String, alert: Alert) = ui {
        if (inForeground && open == pc to key) return@ui
        val (title, text) = when (alert) {
            is Alert.Approval -> app.getString(R.string.alert_approval) to "${alert.tool} — ${alert.summary}"
            is Alert.Question -> app.getString(R.string.alert_question) to alert.question
            is Alert.TurnEnded -> (alert.title.ifBlank { app.getString(R.string.alert_turn_ended) }) to (alert.error ?: alert.last)
        }
        val intent = Intent(app, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_NEW_TASK
            putExtra("pc", pc); putExtra("key", key)
        }
        val pending = android.app.PendingIntent.getActivity(app, key.hashCode(), intent, android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT)
        val notification = NotificationCompat.Builder(app, PoApp.CHANNEL_ALERTS)
            .setSmallIcon(R.drawable.ic_po)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setAutoCancel(true)
            .setContentIntent(pending)
            .build()
        runCatching { app.getSystemService(NotificationManager::class.java)?.notify(key.hashCode(), notification) }
    }
}
