"""
AgentOS Python SDK — Notification Inbox Example

Shows how to read notifications and respond to ask-user questions.

Run:
    python sdk/python/examples/notification_inbox.py
"""
import asyncio
import agentos


async def main() -> None:
    async with await agentos.Agent.connect("inbox-watcher") as agent:
        print(f"Watching inbox for {agent.name}...")
        print("Press Ctrl+C to stop.\n")

        async for msg in agent.inbox:
            print(f"[{msg.priority.upper()}] {msg.subject}")
            if msg.body:
                print(f"  {msg.body[:120]}")

            if msg.is_question():
                print(f"  Question: {msg.question}")
                if msg.options:
                    print(f"  Options: {', '.join(msg.options)}")
                answer = input("  Your answer: ").strip()
                await agent.inbox.respond(msg.id, answer)
                print("  Response sent.")
            else:
                await agent.inbox.mark_read(msg.id)

            print()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\nStopped.")
