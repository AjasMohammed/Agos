"""
Task, TaskResult, TaskStatus — mirrors agentos_types task types.

RunTask in the kernel executes synchronously: the response contains both
the task_id and the result text. This module exposes the result objects.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .types import TaskState, TaskSummary


@dataclass
class TaskResult:
    """Result of a completed task submission."""

    task_id: str
    """The UUID of the task that ran."""

    output: str
    """The text result returned by the agent."""

    status: TaskState = TaskState.Complete
    """Terminal state of the task."""

    extra: dict[str, Any] = field(default_factory=dict)
    """Any additional data fields from the response payload."""

    @property
    def success(self) -> bool:
        """True when the task completed without error."""
        return self.status == TaskState.Complete

    @classmethod
    def from_success_data(cls, data: dict[str, Any]) -> "TaskResult":
        """
        Build a TaskResult from a KernelResponse::Success data dict.

        Expected keys (from cmd_run_task in agentos-kernel):
          task_id  — str UUID
          result   — str (agent answer)
          status   — optional str, "paused" when task is Waiting
        """
        task_id = data.get("task_id", "")
        result_text = data.get("result", "")
        status_str = data.get("status", "")

        if status_str == "paused":
            state = TaskState.Waiting
            result_text = result_text or data.get("reason", "")
        else:
            state = TaskState.Complete

        extra = {k: v for k, v in data.items() if k not in ("task_id", "result", "status")}
        return cls(task_id=task_id, output=result_text, status=state, extra=extra)

    def __str__(self) -> str:
        return self.output
