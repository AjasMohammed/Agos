"""
CrewAI → AgentOS Bridge Example

A CrewAI research crew where agents use AgentOS tools via MCP.
AgentOS acts as the secure execution layer — all tool calls are
validated and audited at the kernel level.

Architecture:
  CrewAI Crew → Agents → MCP tools → AgentOS kernel → Sandboxed execution

Prerequisites:
  pip install -r requirements.txt
  agentos mcp serve --transport http --port 3002 --token your-token &

Usage:
  AGENTOS_TOKEN=your-token python crew.py
"""

import os

from crewai import Agent, Crew, Task
from crewai_tools import MCPServerAdapter


AGENTOS_URL = os.getenv("AGENTOS_URL", "http://localhost:3002/mcp")
AGENTOS_TOKEN = os.getenv("AGENTOS_TOKEN", "")


def build_crew() -> Crew:
    # Connect to AgentOS MCP server and wrap tools for CrewAI
    mcp_config = {
        "url": AGENTOS_URL,
        "transport": "streamable_http",
    }
    if AGENTOS_TOKEN:
        mcp_config["headers"] = {"Authorization": f"Bearer {AGENTOS_TOKEN}"}

    mcp_adapter = MCPServerAdapter(mcp_config)
    agentos_tools = mcp_adapter.tools

    print(f"AgentOS tools loaded: {[t.name for t in agentos_tools]}")

    # Researcher: gathers information using AgentOS file and memory tools
    researcher = Agent(
        role="Research Specialist",
        goal="Research topics thoroughly and save findings to files",
        backstory=(
            "You are an expert researcher with access to AgentOS file and memory tools. "
            "You always save your findings to files for the writer to use."
        ),
        tools=agentos_tools,
        verbose=True,
    )

    # Writer: synthesizes research into a final document
    writer = Agent(
        role="Technical Writer",
        goal="Write clear, accurate summaries based on research findings",
        backstory=(
            "You are a technical writer who reads research files and produces "
            "well-structured, accurate summaries."
        ),
        tools=agentos_tools,
        verbose=True,
    )

    research_task = Task(
        description=(
            "Research the current state of AI agent security in 2026. "
            "Save your findings to research_notes.txt in the workspace."
        ),
        expected_output="A research_notes.txt file with key findings",
        agent=researcher,
    )

    write_task = Task(
        description=(
            "Read research_notes.txt and write a 2-paragraph summary "
            "suitable for a technical blog post. Save it to summary.txt."
        ),
        expected_output="A summary.txt file with the final summary",
        agent=writer,
    )

    return Crew(
        agents=[researcher, writer],
        tasks=[research_task, write_task],
        verbose=True,
    )


if __name__ == "__main__":
    crew = build_crew()
    result = crew.kickoff()
    print("\n=== Final Result ===")
    print(result)
