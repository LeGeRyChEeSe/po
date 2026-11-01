package dev.po.remote

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.speech.tts.TextToSpeech
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.app.NotificationCompat
import dev.po.remote.ui.AppTheme
import dev.po.remote.ui.Appearance
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
import uniffi.po_mobile.SubagentRec
import java.util.Locale

/** Where the user is. */
sealed class Screen {
    data object SignIn : Screen()
    data object Home : Screen()
    data class Project(val pc: String, val path: String, val name: String) : Screen()
    data class Conversation(val pc: String, val key: String) : Screen()
    data object Settings : Screen()
}

/** A file sent to the PC, waiting in the input to go with the next message. */
data class Attachment(val name: String, val mention: String, val image: Boolean)

/** A key of a session still being opened on the PC: the screen shows it loading meanwhile. */
const val OPENING = "opening:"

/**
 * Everything the screens show, kept as Compose state, and the actions on the core. The core's
 * calls block: they run on the app's threads, and their results come back on the main one.
 */
class AppState(private val app: PoApp) : Listener {
    private val main = Handler(Looper.getMainLooper())
    private val prefs = app.getSharedPreferences("po", Context.MODE_PRIVATE)

    var account by mutableStateOf<String?>(null)
    var signIn by mutableStateOf<SignIn?>(null)
    var busyText by mutableStateOf<String?>(null)
    var error by mutableStateOf<String?>(null)
    var toast by mutableStateOf<String?>(null)

    var theme by mutableStateOf(AppTheme.of(prefs.getString("theme", null)))
        private set
    var appearance by mutableStateOf(Appearance.of(prefs.getString("appearance", null)))
        private set

    var pcs by mutableStateOf<List<PcInfo>>(emptyList())
    var ready by mutableStateOf(true)
    var refreshing by mutableStateOf(false)
    val links = mutableStateMapOf<String, LinkState>()
    val projects = mutableStateMapOf<String, List<ProjectRec>>()
    val projectsLoading = mutableStateMapOf<String, Boolean>()
    var sessions by mutableStateOf<List<SessionRec>>(emptyList())
    var sessionsLoading by mutableStateOf(false)

    // The conversation shown.
    var open by mutableStateOf<Pair<String, String>?>(null)
    val rows = mutableStateListOf<ItemRec>()
    var status by mutableStateOf<StatusRec?>(null)
    var running by mutableStateOf(false)
    var approval by mutableStateOf<ApprovalRec?>(null)
    var question by mutableStateOf<QuestionRec?>(null)
    var closed by mutableStateOf(false)
    var subagents by mutableStateOf<List<SubagentRec>>(emptyList())
    /** Nothing received yet for the conversation shown: the screen says it is loading. */
    var loading by mutableStateOf(false)
    var commands by mutableStateOf<List<CommandRec>>(emptyList())
    var draft by mutableStateOf("")
    val attachments = mutableStateListOf<Attachment>()
    var uploading by mutableStateOf(0)

    // A command's menu: what it shows, the line it was built from (a live one is asked again),
    // and whether it is being asked.
    var menu by mutableStateOf<MenuRec?>(null)
    var menuLine by mutableStateOf<String?>(null)
    var menuLoading by mutableStateOf(false)

    // Dictation, transcribed by the PC.
    var recording by mutableStateOf(false)
    var transcribing by mutableStateOf(false)
    private var recorder: VoiceRecorder? = null

    // Reading answers aloud, on the phone.
    private var tts: TextToSpeech? = null
    private var ttsReady = false
    var speaking by mutableStateOf<String?>(null)

    val stack = mutableStateListOf<Screen>(Screen.Home)
    val screen: Screen get() = stack.last()
    var inForeground = true

    private fun ui(block: () -> Unit) {
        main.post(block)
    }

    /** Runs a call of the core off the main thread; its result (or its error) comes back on it. */
    private fun <T> work(label: String?, call: () -> T, failed: (Throwable) -> Unit = {}, done: (T) -> Unit = {}) {
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
                    failed(it)
                }
            }
        }
    }

    fun describe(e: Throwable): String = when (e) {
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
        if (menu != null) {
            menuBack()
            return true
        }
        if (stack.size <= 1) return false
        val left = stack.removeAt(stack.size - 1)
        if (left is Screen.Conversation) closeConversation()
        return true
    }

    fun chooseTheme(value: AppTheme) {
        theme = value
        prefs.edit().putString("theme", value.key).apply()
    }

    fun chooseAppearance(value: Appearance) {
        appearance = value
        prefs.edit().putString("appearance", value.key).apply()
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
        projectsLoading[pc] = true
        app.io.execute {
            val result = runCatching { app.core.projects(pc) }
            ui {
                projectsLoading[pc] = false
                result.onSuccess { projects[pc] = it }
            }
        }
    }

    fun openProject(pc: String, project: ProjectRec) {
        sessions = emptyList()
        go(Screen.Project(pc, project.path, project.name))
        loadSessions(pc, project.path)
    }

    fun loadSessions(pc: String, path: String) {
        sessionsLoading = true
        work(null, { app.core.sessions(pc, path) }, failed = { sessionsLoading = false }) {
            sessions = it
            sessionsLoading = false
        }
    }

    /** Removes a saved session from the PC's lists (it goes to the PC's bin of sessions). */
    fun forget(pc: String, session: SessionRec) {
        val saved = session.savedId ?: return
        work(null, { app.core.forget(pc, saved) }) { text ->
            sessions = sessions.filter { it.key != session.key }
            toast = text
        }
    }

    fun openConversation(pc: String, key: String) {
        closeConversation()
        resetConversation(pc, key)
        go(Screen.Conversation(pc, key))
        follow(pc, key)
    }

    private fun resetConversation(pc: String, key: String) {
        open = pc to key
        rows.clear(); status = null; running = false; approval = null; question = null; closed = false
        subagents = emptyList(); loading = true
        menu = null; menuLine = null
        attachments.clear()
    }

    private fun follow(pc: String, key: String) {
        app.io.execute { app.core.open(pc, key) }
        work(null, { app.core.commands(pc, key) }) { commands = it }
        // Kept connected while the app is in the background, to hear the end of a turn.
        app.startForegroundService(Intent(app, LinkService::class.java))
    }

    fun closeConversation() {
        val (pc, key) = open ?: return
        open = null
        stopSpeaking()
        if (!key.startsWith(OPENING)) app.io.execute { app.core.closeView(pc, key) }
    }

    /**
     * A new session: its screen opens at once, loading while the PC starts it, and everything can
     * be done there before any message (a setting, a command).
     */
    fun newSession(pc: String, project: String) {
        closeConversation()
        val pending = OPENING + project
        resetConversation(pc, pending)
        go(Screen.Conversation(pc, pending))
        work(null, { app.core.newSession(pc, project, null) }, failed = {
            if (open == pc to pending) back()
        }) { key ->
            if (open != pc to pending) {
                // Left meanwhile: the session stays open on the PC, nothing to follow.
                return@work
            }
            val at = stack.indexOfLast { it is Screen.Conversation && it.key == pending }
            if (at >= 0) stack[at] = Screen.Conversation(pc, key)
            open = pc to key
            follow(pc, key)
        }
    }

    fun openSaved(pc: String, session: SessionRec) {
        // A saved session is read as it was, until the user writes in it (then it opens again).
        openConversation(pc, session.key)
    }

    fun resumeSaved(pc: String, saved: String, then: (String) -> Unit = {}) {
        work(app.getString(R.string.busy_opening), { app.core.resumeSession(pc, saved) }) { key ->
            if (stack.lastOrNull() is Screen.Conversation) stack.removeAt(stack.size - 1)
            openConversation(pc, key)
            then(key)
        }
    }

    fun send(input: InputRec, after: (String) -> Unit = {}) {
        val (pc, key) = open ?: return
        if (key.startsWith(OPENING)) {
            toast = app.getString(R.string.still_opening)
            return
        }
        work(null, { app.core.send(pc, key, input) }, done = after)
    }

    /** What the input holds, sent: a command (its menu first), or a message with its files. */
    fun submit() {
        val typed = draft.trim()
        val mentions = attachments.joinToString(" ") { it.mention }
        val text = listOf(typed, mentions).filter { it.isNotEmpty() }.joinToString("\n")
        if (text.isEmpty()) return
        val (pc, key) = open ?: return
        if (key.startsWith(OPENING)) {
            toast = app.getString(R.string.still_opening)
            return
        }
        draft = ""
        attachments.clear()
        val command = typed.startsWith("/") && mentions.isEmpty()
        // A saved session opens again (without a terminal) for the message to go in.
        if (key.startsWith("saved:")) {
            resumeSaved(pc, key.removePrefix("saved:")) { if (command) command(typed) else send(InputRec.Prompt(text)) }
            return
        }
        if (command) command(typed) else send(InputRec.Prompt(text))
    }

    /** A command: its menu when it needs a choice, else it runs and says what it did. */
    fun command(line: String) {
        val (pc, key) = open ?: return
        if (key.startsWith(OPENING) || key.startsWith("saved:")) {
            toast = app.getString(R.string.still_opening)
            return
        }
        menuLoading = true
        app.io.execute {
            val result = runCatching { app.core.menu(pc, key, line) }
            ui {
                menuLoading = false
                val found = result.getOrNull()
                if (found != null && found.items.isNotEmpty()) {
                    menu = found
                    menuLine = line
                } else {
                    menu = null
                    menuLine = null
                    send(InputRec.Command(line)) { answer ->
                        if (answer.isNotBlank()) toast = answer.lines().first()
                    }
                }
            }
        }
    }

    /** A live menu is asked again: what it shows changes by itself. */
    fun refreshMenu() {
        val line = menuLine ?: return
        val (pc, key) = open ?: return
        if (menu?.live != true || menuLoading) return
        app.io.execute {
            val result = runCatching { app.core.menu(pc, key, line) }
            ui {
                val found = result.getOrNull() ?: return@ui
                // Closed or replaced meanwhile: nothing to refresh.
                if (menuLine == line && menu != null) menu = found
            }
        }
    }

    fun closeMenu() {
        menu = null
        menuLine = null
    }

    /** Back from a menu: to the menu above it, or closed. */
    fun menuBack() {
        val back = menu?.back
        closeMenu()
        if (back != null) command(back)
    }

    fun chooseMenuItem(action: String, line: String, then: String?) {
        closeMenu()
        when (action) {
            "fill" -> draft = line
            "show" -> send(InputRec.Command(line)) { answer ->
                if (answer.isNotBlank()) toast = answer.lines().first()
                if (then != null) command(then)
            }
            else -> {
                if (then == null) {
                    command(line)
                } else {
                    // The screen comes back once its command has run: the user sees the change.
                    send(InputRec.Command(line)) { answer ->
                        if (answer.isNotBlank()) toast = answer.lines().first()
                        command(then)
                    }
                }
            }
        }
    }

    fun closeSession() {
        val (pc, key) = open ?: return
        work(null, { app.core.closeSession(pc, key) }) { back() }
    }

    // Stop, pause, the messages waiting.

    fun stop() = send(InputRec.Interrupt)
    fun pause() = send(InputRec.Pause)
    fun resume() = send(InputRec.Resume)
    fun unqueue(index: Int) = send(InputRec.Unqueue(index.toUInt()))
    fun requeue(index: Int, text: String) = send(InputRec.Requeue(index.toUInt(), text))

    // Files: sent to the session's folder on the PC, then mentioned in the message.

    fun attach(uri: Uri) {
        val (pc, key) = open ?: return
        if (key.startsWith(OPENING)) {
            toast = app.getString(R.string.still_opening)
            return
        }
        uploading += 1
        app.io.execute {
            val result = runCatching {
                val file = Uploads.read(app, uri)
                Attachment(file.name, app.core.upload(pc, key, file.name, file.bytes), file.image)
            }
            ui {
                uploading -= 1
                result.onSuccess { attachments.add(it) }.onFailure { error = describe(it) }
            }
        }
    }

    fun detach(attachment: Attachment) {
        attachments.remove(attachment)
    }

    // Dictation: the phone records, the PC's own whisper writes it down — no third party.

    fun toggleRecording(context: Context) {
        if (recording) {
            val wav = recorder?.stop()
            recorder = null
            recording = false
            val (pc, _) = open ?: return
            if (wav == null || wav.size < 16000) {
                toast = app.getString(R.string.voice_too_short)
                return
            }
            transcribing = true
            app.io.execute {
                val result = runCatching { app.core.transcribe(pc, wav) }
                ui {
                    transcribing = false
                    result.onSuccess { heard -> draft = (draft.trimEnd() + " " + heard.trim()).trim() }
                        .onFailure { error = describe(it) }
                }
            }
            return
        }
        val started = runCatching { VoiceRecorder(context).also { it.start() } }
        started.onSuccess { recorder = it; recording = true }
            .onFailure { error = app.getString(R.string.voice_no_mic, it.message ?: "") }
    }

    // Reading an answer aloud, by the phone's own speech engine.

    fun speak(text: String) {
        if (speaking == text) {
            stopSpeaking()
            return
        }
        val say = {
            speaking = text
            val engine = tts
            engine?.language = Locale.getDefault()
            engine?.speak(Speech.plain(text), TextToSpeech.QUEUE_FLUSH, null, "po")
        }
        if (tts != null && ttsReady) {
            say()
            return
        }
        tts = TextToSpeech(app) { status ->
            ui {
                ttsReady = status == TextToSpeech.SUCCESS
                if (ttsReady) {
                    tts?.setOnUtteranceProgressListener(object : android.speech.tts.UtteranceProgressListener() {
                        override fun onStart(id: String?) {}
                        override fun onDone(id: String?) = ui { speaking = null }
                        @Deprecated("Deprecated in Java")
                        @Suppress("OVERRIDE_DEPRECATION")
                        override fun onError(id: String?) = ui { speaking = null }
                    })
                    say()
                } else {
                    error = app.getString(R.string.speak_unavailable)
                }
            }
        }
    }

    fun stopSpeaking() {
        tts?.stop()
        speaking = null
    }

    // What the core says on its own.

    override fun onSession(pc: String, key: String, update: SessionUpdate) = ui {
        if (open != pc to key) return@ui
        loading = false
        // Rows before `from` are unchanged: only the end is replaced.
        while (rows.size > update.from.toInt()) rows.removeAt(rows.size - 1)
        rows.addAll(update.items)
        status = update.status
        running = update.busy
        approval = update.approval
        question = update.question
        closed = update.closed
        update.subagents?.let { subagents = it }
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

/** What a spoken answer says: the text without its markdown marks. */
object Speech {
    fun plain(text: String): String = text
        .replace(Regex("```[\\s\\S]*?```"), " ")
        .replace(Regex("\\[([^\\]]+)]\\([^)]+\\)"), "$1")
        .replace(Regex("[*_`#>]+"), "")
        .replace(Regex("\\s+"), " ")
        .trim()
}
