package com.chatx.monitor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MonitorPollingContinuityTest {
    @Test
    fun pollerIsStalledWhenNoCycleCompletesForNinetySeconds() {
        assertFalse(MonitorPollWatchdog.isStalled(1_000L, 91_000L, 10L))
        assertTrue(MonitorPollWatchdog.isStalled(1_000L, 91_001L, 10L))
        assertFalse(MonitorPollWatchdog.isStalled(0L, 500_000L, 10L))
    }

    @Test
    fun slowPollingIntervalGetsLongerWatchdogWindow() {
        assertFalse(MonitorPollWatchdog.isStalled(1_000L, 181_000L, 60L))
        assertTrue(MonitorPollWatchdog.isStalled(1_000L, 181_001L, 60L))
    }

    @Test
    fun repeatedSnapshotAndLateRelayResultAreNotPublishedAgain() {
        assertTrue(MonitorSnapshotFreshness.shouldRejectNonNewer(
            200_000L, 201_000L, 200_000L, 202_000L, 30_000L,
        ))
        assertTrue(MonitorSnapshotFreshness.shouldRejectNonNewer(
            200_000L, 201_000L, 199_000L, 202_000L, 30_000L,
        ))
        assertFalse(MonitorSnapshotFreshness.shouldRejectNonNewer(
            200_000L, 201_000L, 201_000L, 202_000L, 30_000L,
        ))
        assertFalse(MonitorSnapshotFreshness.shouldRejectNonNewer(
            200_000L, 201_000L, 199_000L, 240_000L, 30_000L,
        ))
    }

    @Test
    fun manualRefreshWaitsForControlAndPublishesOnlyOnce() {
        val selector = ManualRefreshSelector()
        assertNull(selector.complete(false, "cached"))
        assertEquals("live", selector.complete(true, "live")?.json)
        assertNull(selector.complete(false, "old"))
    }

    @Test
    fun unsupportedControlFallsBackToHttpsWithoutDoubleRendering() {
        val selector = ManualRefreshSelector()
        assertNull(selector.complete(true, null))
        assertEquals("cached", selector.complete(false, "cached")?.json)
        assertNull(selector.complete(true, "later"))
    }

    @Test
    fun refreshFailureIsOnlyReportedAfterBothPathsFail() {
        val selector = ManualRefreshSelector()
        assertNull(selector.complete(false, null))
        val final = selector.complete(true, null)
        assertNotNull(final)
        assertNull(final?.json)
        assertNull(selector.complete(true, "later"))
    }

    @Test
    fun successfulControlNeverGetsReplacedByDelayedCache() {
        val selector = ManualRefreshSelector()
        assertEquals("live", selector.complete(true, "live")?.json)
        assertNull(selector.complete(false, "cached"))
    }

    @Test
    fun olderCachedSnapshotDoesNotOverrideFreshSnapshot() {
        assertTrue(MonitorSnapshotFreshness.shouldRejectOlder(
            previousServerTime = 200_000L,
            previousReceivedAt = 200_000L,
            serverTime = 199_000L,
            now = 201_000L,
            staleAfterMs = 30_000L,
        ))
    }

    @Test
    fun timestampRegressionCannotFreezeMonitorForever() {
        assertFalse(MonitorSnapshotFreshness.shouldRejectOlder(
            previousServerTime = 300_000L,
            previousReceivedAt = 200_000L,
            serverTime = 199_000L,
            now = 231_000L,
            staleAfterMs = 30_000L,
        ))
        assertFalse(MonitorSnapshotFreshness.shouldRejectOlder(
            previousServerTime = 300_000L,
            previousReceivedAt = 200_000L,
            serverTime = 199_000L,
            now = 150_000L,
            staleAfterMs = 30_000L,
        ))
    }
}
