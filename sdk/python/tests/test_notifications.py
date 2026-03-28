"""
Tests for the Inbox / notification system.

Covers: list messages, mark read, respond to question, async iteration.
"""
from __future__ import annotations

import asyncio

import pytest

from agentos import MockKernel
from agentos.notification import Message


class TestInboxMessages:
    async def test_empty_inbox_initially(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("notify-agent")
            msgs = await agent.inbox.messages()
            assert msgs == []

    async def test_push_and_list_notification(self):
        async with MockKernel() as kernel:
            kernel.push_notification(
                subject="Hello",
                body="World",
                priority="info",
            )
            agent = await kernel.connect_agent("agent")
            msgs = await agent.inbox.messages(unread_only=False)
            assert len(msgs) == 1
            assert msgs[0].subject == "Hello"
            assert msgs[0].body == "World"
            assert msgs[0].priority == "info"
            assert msgs[0].read is False

    async def test_push_multiple_notifications(self):
        async with MockKernel() as kernel:
            for i in range(3):
                kernel.push_notification(subject=f"msg-{i}", body=f"body-{i}")
            agent = await kernel.connect_agent("agent")
            msgs = await agent.inbox.messages(unread_only=False)
            assert len(msgs) == 3

    async def test_unread_only_filter(self):
        async with MockKernel() as kernel:
            nid = kernel.push_notification(subject="unread")
            # Mark it read via handler
            kernel._notifications[0].read = True

            agent = await kernel.connect_agent("agent")
            msgs = await agent.inbox.messages(unread_only=True)
            assert len(msgs) == 0

            msgs_all = await agent.inbox.messages(unread_only=False)
            assert len(msgs_all) == 1


class TestInboxMarkRead:
    async def test_mark_read(self):
        async with MockKernel() as kernel:
            nid = kernel.push_notification(subject="unread test")
            agent = await kernel.connect_agent("agent")

            # Before mark_read — appears in unread list
            msgs = await agent.inbox.messages(unread_only=True)
            assert len(msgs) == 1

            await agent.inbox.mark_read(nid)

            # After mark_read — not in unread list
            msgs = await agent.inbox.messages(unread_only=True)
            assert len(msgs) == 0


class TestInboxRespond:
    async def test_respond_stores_response(self):
        async with MockKernel() as kernel:
            nid = kernel.push_notification(
                kind="Question", subject="Deploy now?", body="Should I deploy?"
            )
            agent = await kernel.connect_agent("agent")

            msgs = await agent.inbox.messages(unread_only=False)
            assert len(msgs) == 1
            assert msgs[0].kind == "Question"
            assert msgs[0].id == nid

            await agent.inbox.respond(nid, "yes")

            stored = kernel.get_notification_response(nid)
            assert stored == "yes"

    async def test_respond_marks_notification_read(self):
        async with MockKernel() as kernel:
            nid = kernel.push_notification(kind="Question", subject="Q?")
            agent = await kernel.connect_agent("agent")

            await agent.inbox.respond(nid, "ok")

            # Notification should be marked read
            unread = await agent.inbox.messages(unread_only=True)
            assert len(unread) == 0


class TestInboxAsyncIter:
    async def test_async_iter_yields_3_messages(self):
        """Push 3 notifications, iterate inbox, assert all 3 received."""
        async with MockKernel() as kernel:
            for i in range(3):
                kernel.push_notification(subject=f"n{i}")

            agent = await kernel.connect_agent("agent")
            received: list[Message] = []

            async def collect():
                async for msg in agent.inbox:
                    received.append(msg)
                    if len(received) >= 3:
                        break

            await asyncio.wait_for(collect(), timeout=5.0)
            assert len(received) == 3
            subjects = {m.subject for m in received}
            assert subjects == {"n0", "n1", "n2"}

    async def test_async_iter_deduplicates(self):
        """Messages already seen are not yielded again."""
        async with MockKernel() as kernel:
            # Use a custom kernel with poll_interval=0 to speed up test
            from agentos.notification import Inbox
            from agentos.testing.mock_kernel import MockBusClient

            agent = await kernel.connect_agent("dedup-agent")
            # Patch inbox poll interval to 0
            inbox = Inbox(agent._client, poll_interval=0)

            nid = kernel.push_notification(subject="once")
            received = []

            async def collect_two_polls():
                async for msg in inbox:
                    received.append(msg)
                    if len(received) >= 1:
                        # Do one more poll cycle — shouldn't yield duplicates
                        break

            await asyncio.wait_for(collect_two_polls(), timeout=2.0)
            assert len(received) == 1
