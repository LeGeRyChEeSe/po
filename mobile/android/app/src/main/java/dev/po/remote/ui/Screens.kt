package dev.po.remote.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.po.remote.AppState
import dev.po.remote.BuildConfig
import dev.po.remote.R
import dev.po.remote.Screen
import uniffi.po_mobile.LinkState
import uniffi.po_mobile.PcInfo
import uniffi.po_mobile.SessionRec

@Composable
fun App(state: AppState) {
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.error) {
        state.error?.let { snackbar.showSnackbar(it); state.error = null }
    }
    LaunchedEffect(state.toast) {
        state.toast?.let { snackbar.showSnackbar(it); state.toast = null }
    }
    Box(Modifier.fillMaxSize()) {
        when (val screen = state.screen) {
            Screen.SignIn -> SignInScreen(state, snackbar)
            Screen.Home -> HomeScreen(state, snackbar)
            is Screen.Project -> ProjectScreen(state, screen, snackbar)
            is Screen.Conversation -> ConversationScreen(state, screen, snackbar)
            Screen.Settings -> SettingsScreen(state, snackbar)
        }
        state.busyText?.let { text ->
            Surface(Modifier.align(Alignment.BottomCenter).padding(bottom = 96.dp), shape = MaterialTheme.shapes.large, tonalElevation = 6.dp, shadowElevation = 6.dp) {
                Row(Modifier.padding(horizontal = 18.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(12.dp))
                    Text(text)
                }
            }
        }
    }
}

@Composable
private fun SignInScreen(state: AppState, snackbar: SnackbarHostState) {
    val context = LocalContext.current
    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(28.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Mascot(120.dp)
            Spacer(Modifier.height(20.dp))
            Text(stringResource(R.string.welcome_title), style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
            Spacer(Modifier.height(10.dp))
            Text(stringResource(R.string.welcome_text), textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(28.dp))
            val code = state.signIn
            if (code == null) {
                Button(onClick = { state.beginSignIn() }, enabled = state.busyText == null) { Text(stringResource(R.string.sign_in)) }
            } else {
                Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
                    Column(Modifier.padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(stringResource(R.string.sign_in_code))
                        Spacer(Modifier.height(8.dp))
                        Text(code.code, fontSize = 30.sp, fontWeight = FontWeight.Bold, letterSpacing = 3.sp)
                        Spacer(Modifier.height(12.dp))
                        Button(onClick = { copyAndOpen(context, code.code, code.url) }) { Text(stringResource(R.string.sign_in_open)) }
                        Spacer(Modifier.height(8.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(8.dp))
                            Text(stringResource(R.string.sign_in_waiting), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            }
            Spacer(Modifier.height(20.dp))
            Text(stringResource(R.string.sign_in_note), style = MaterialTheme.typography.bodySmall, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (BuildConfig.DEBUG) {
                TextButton(onClick = { state.go(Screen.Settings) }) { Text(stringResource(R.string.settings)) }
            }
        }
    }
}

private fun copyAndOpen(context: Context, code: String, url: String) {
    val clipboard = context.getSystemService(android.content.ClipboardManager::class.java)
    clipboard?.setPrimaryClip(android.content.ClipData.newPlainText("code", code))
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HomeScreen(state: AppState, snackbar: SnackbarHostState) {
    LaunchedEffect(Unit) { state.refresh() }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Mascot(30.dp)
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text("Po", fontWeight = FontWeight.Bold)
                            Text(state.account ?: "", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                actions = {
                    IconButton(onClick = { state.refresh() }) { Icon(Icons.Default.Refresh, stringResource(R.string.refresh)) }
                    IconButton(onClick = { state.go(Screen.Settings) }) { Icon(Icons.Default.Settings, stringResource(R.string.settings)) }
                },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (state.pcs.isEmpty()) {
                item {
                    Empty(if (state.refreshing) stringResource(R.string.loading) else stringResource(R.string.no_pc))
                }
            }
            if (!state.ready && state.pcs.isNotEmpty()) {
                item { Hint(stringResource(R.string.waiting_for_pc)) }
            }
            items(state.pcs, key = { it.id }) { pc -> PcCard(state, pc) }
        }
    }
}

@Composable
private fun PcCard(state: AppState, pc: PcInfo) {
    val link = state.links[pc.id]
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                val dot = when (link) {
                    is LinkState.Up -> PoColors.Moss
                    is LinkState.Connecting -> PoColors.Amber
                    else -> if (pc.reachable) Color.Gray else PoColors.Red
                }
                Box(Modifier.size(10.dp).clip(CircleShape).background(dot))
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(pc.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text(
                        when (link) {
                            is LinkState.Up -> stringResource(if (link.punched) R.string.link_up_punched else R.string.link_up, link.address)
                            is LinkState.Connecting -> stringResource(R.string.link_connecting)
                            is LinkState.Down -> stringResource(R.string.link_down, link.reason)
                            null -> if (pc.reachable) stringResource(R.string.seen, ago(pc.seen)) else stringResource(R.string.pc_not_ready)
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                    )
                }
            }
            val projects = state.projects[pc.id]
            if (projects != null) {
                Spacer(Modifier.height(10.dp))
                HorizontalDivider()
                if (projects.isEmpty()) Text(stringResource(R.string.no_project), Modifier.padding(top = 10.dp), style = MaterialTheme.typography.bodySmall)
                projects.forEach { p ->
                    Row(
                        Modifier.fillMaxWidth().clickable { state.go(Screen.Project(pc.id, p.path, p.name)); state.loadSessions(pc.id, p.path) }.padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(p.name, fontWeight = FontWeight.Medium)
                            Text(p.path, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                        if (p.live > 0u) Badge(stringResource(R.string.live_count, p.live.toInt()), MaterialTheme.colorScheme.tertiary)
                        Spacer(Modifier.width(6.dp))
                        Badge(p.saved.toString(), MaterialTheme.colorScheme.secondary)
                    }
                }
            } else if (pc.reachable) {
                TextButton(onClick = { state.loadProjects(pc.id) }) { Text(stringResource(R.string.connect)) }
            }
        }
    }
}

@Composable
fun Badge(text: String, color: Color) {
    Surface(shape = CircleShape, color = color.copy(alpha = 0.16f)) {
        Text(text, Modifier.padding(horizontal = 8.dp, vertical = 2.dp), color = color, style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
fun Empty(text: String) {
    Column(Modifier.fillMaxWidth().padding(top = 60.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Mascot(80.dp)
        Spacer(Modifier.height(14.dp))
        Text(text, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
fun Hint(text: String) {
    Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
        Text(text, Modifier.padding(14.dp), style = MaterialTheme.typography.bodyMedium)
    }
}

fun ago(seconds: ULong): String {
    val delta = (System.currentTimeMillis() / 1000 - seconds.toLong()).coerceAtLeast(0)
    return when {
        delta < 60 -> "< 1 min"
        delta < 3600 -> "${delta / 60} min"
        delta < 86400 -> "${delta / 3600} h"
        else -> "${delta / 86400} j"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProjectScreen(state: AppState, screen: Screen.Project, snackbar: SnackbarHostState) {
    var asking by remember { mutableStateOf(false) }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Column { Text(screen.name); Text(screen.path, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis) } },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                actions = { IconButton(onClick = { state.loadSessions(screen.pc, screen.path) }) { Icon(Icons.Default.Refresh, stringResource(R.string.refresh)) } },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(onClick = { asking = true }, icon = { Icon(Icons.Default.Add, null) }, text = { Text(stringResource(R.string.new_session)) })
        },
    ) { padding ->
        val list = state.sessions.filter { it.project.equals(screen.path, ignoreCase = true) || it.project.replace('\\', '/').equals(screen.path.replace('\\', '/'), ignoreCase = true) }
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (list.isEmpty()) item { Empty(stringResource(R.string.no_session)) }
            items(list, key = { it.key }) { s -> SessionRow(s) { state.openSaved(screen.pc, s) } }
        }
    }
    if (asking) {
        var first by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { asking = false },
            title = { Text(stringResource(R.string.new_session)) },
            text = {
                Column {
                    Text(stringResource(R.string.new_session_text), style = MaterialTheme.typography.bodySmall)
                    Spacer(Modifier.height(10.dp))
                    OutlinedTextField(first, { first = it }, Modifier.fillMaxWidth(), placeholder = { Text(stringResource(R.string.first_message)) })
                }
            },
            confirmButton = { Button(onClick = { asking = false; state.newSession(screen.pc, screen.path, first) }) { Text(stringResource(R.string.open)) } },
            dismissButton = { TextButton(onClick = { asking = false }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

@Composable
private fun SessionRow(s: SessionRec, onClick: () -> Unit) {
    Card(Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(s.title.ifBlank { stringResource(R.string.untitled) }, fontWeight = FontWeight.Medium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                val detail = listOfNotNull(
                    s.model.takeIf { it.isNotBlank() },
                    stringResource(R.string.exchanges, s.exchanges.toInt()),
                    s.updated.takeIf { it.isNotBlank() }?.take(16)?.replace('T', ' '),
                ).joinToString(" · ")
                Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            when {
                s.busy -> Badge(stringResource(R.string.working), PoColors.Amber)
                s.live && s.headless -> Badge(stringResource(R.string.from_phone), MaterialTheme.colorScheme.primary)
                s.live -> Badge(stringResource(R.string.in_terminal), MaterialTheme.colorScheme.tertiary)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScreen(state: AppState, snackbar: SnackbarHostState) {
    val context = LocalContext.current
    val prefs = remember { context.getSharedPreferences("po", Context.MODE_PRIVATE) }
    var api by remember { mutableStateOf(prefs.getString("github_api", "") ?: "") }
    var web by remember { mutableStateOf(prefs.getString("github_web", "") ?: "") }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings)) },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Text(stringResource(R.string.account), style = MaterialTheme.typography.titleMedium)
            Text(state.account ?: stringResource(R.string.not_signed_in))
            if (state.account != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    OutlinedButton(onClick = { state.signOut() }) { Text(stringResource(R.string.sign_out)) }
                    OutlinedButton(onClick = { state.signOut(); state.beginSignIn() }) { Text(stringResource(R.string.switch_account)) }
                }
                Text(stringResource(R.string.sign_out_note), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            HorizontalDivider()
            Text(stringResource(R.string.this_phone), style = MaterialTheme.typography.titleMedium)
            Text(dev.po.remote.PoApp.instance.core.deviceId(), style = Mono)
            if (BuildConfig.DEBUG) {
                HorizontalDivider()
                Text(stringResource(R.string.debug_github), style = MaterialTheme.typography.titleMedium)
                OutlinedTextField(api, { api = it }, Modifier.fillMaxWidth(), label = { Text("API") }, placeholder = { Text("http://10.0.2.2:18998") })
                OutlinedTextField(web, { web = it }, Modifier.fillMaxWidth(), label = { Text("Web") }, placeholder = { Text("http://10.0.2.2:18998") })
                Button(onClick = {
                    prefs.edit().putString("github_api", api.trim()).putString("github_web", web.trim()).apply()
                    state.toast = context.getString(R.string.restart_to_apply)
                }) { Text(stringResource(R.string.save)) }
            }
            Spacer(Modifier.weight(1f))
            Text("Po ${BuildConfig.VERSION_NAME}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
