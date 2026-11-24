package dev.po.remote.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.po_mobile.ItemKind
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.ToolStatus

class GroupingTest {
    private fun row(kind: ItemKind, text: String, title: String = "", status: ToolStatus = ToolStatus.NONE, streaming: Boolean = false) =
        ItemRec(kind, text, title, "", "", status, streaming, 0uL, emptyList())

    /** A turn that talks between its tools, as local models do. */
    private fun chattyTurn() = listOf(
        row(ItemKind.USER, "fix the map"),
        row(ItemKind.THINKING, "where is the map"),
        row(ItemKind.TOOL, "read a.ts", "read", ToolStatus.NONE),
        row(ItemKind.ASSISTANT, "I clean the test map now."),
        row(ItemKind.TOOL, "edit modifié a.ts", "edit", ToolStatus.NONE),
        row(ItemKind.ASSISTANT, "Rebuild and final check."),
        row(ItemKind.TOOL, "bash npm run build", "bash", ToolStatus.NONE),
        row(ItemKind.ASSISTANT, "Done: the map is cleaned."),
    )

    @Test
    fun lowKeepsOnlyTheFinalAnswerOfAFinishedTurn() {
        val blocks = groupRows(chattyTurn(), Verbosity.Low, running = false)
        val singles = blocks.filterIsInstance<DisplayItem.Single>().map { it.item.text }
        assertEquals(listOf("fix the map", "Done: the map is cleaned."), singles)
        val work = blocks.filterIsInstance<DisplayItem.LowWork>().single()
        assertTrue(work.answered && !work.live)
        assertEquals(6, work.items.size)
    }

    @Test
    fun lowShowsOneLiveLineWhileTheTurnRuns() {
        val rows = chattyTurn().dropLast(1) + row(ItemKind.TOOL, "bash npm test", "bash", ToolStatus.RUNNING)
        val blocks = groupRows(rows, Verbosity.Low, running = true)
        assertEquals(2, blocks.size)
        val work = blocks.last() as DisplayItem.LowWork
        assertTrue(work.live && !work.answered)
        // A message being written at the end is still work: a tool may follow it.
        val writing = groupRows(chattyTurn(), Verbosity.Low, running = true)
        assertTrue((writing.last() as DisplayItem.LowWork).live)
    }

    @Test
    fun aCutTurnKeepsItsWork() {
        val blocks = groupRows(chattyTurn().take(7), Verbosity.Low, running = false)
        val work = blocks.last() as DisplayItem.LowWork
        assertTrue(!work.answered && !work.live)
    }

    @Test
    fun mediumKeepsTheMessagesAndHighEveryStep() {
        val medium = groupRows(chattyTurn(), Verbosity.Medium)
        assertEquals(3, medium.filterIsInstance<DisplayItem.Single>().count { it.item.kind == ItemKind.ASSISTANT })
        val high = groupRows(chattyTurn(), Verbosity.High)
        assertEquals(chattyTurn().size, high.size)
    }

    @Test
    fun formerStoredLevelsKeepWhatTheyShowed() {
        assertEquals(Verbosity.Medium, Verbosity.of("quiet"))
        assertEquals(Verbosity.Medium, Verbosity.of("normal"))
        assertEquals(Verbosity.High, Verbosity.of("verbose"))
        assertEquals(Verbosity.Low, Verbosity.of("low"))
        assertEquals(Verbosity.Medium, Verbosity.of(null))
    }
}
