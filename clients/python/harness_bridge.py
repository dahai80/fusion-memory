"""fusion-harness MemoryBridge adapters over fm-server HTTP (issue #42, plan P3-2).

Implements fusion_harness.memory_link protocols structurally (runtime_checkable
Protocol — no import of fusion_harness required):

  HarnessMemorySink   → MemorySink.store(key, payload, metadata=None)
                        compaction summaries persist via MemoryBridge.persist_compaction
  HarnessMemorySource → MemorySource.retrieve(query, limit=5)
                        just-in-time recall via MemoryBridge.recall / inject_context

Retrieval is bounded (top_k=limit) and failure-tolerant: MemoryBridge already
wraps both calls in try/except (recall is an upgrade, never a dependency), so
these adapters surface errors instead of swallowing them — except network
outages on the source path, which degrade to [] (bounded no-op) per the
issue contract. install: copy this file into the consumer (same vendoring
model as fusion_memory_client.py). Requires httpx.

env (operator configuration):
  FUSION_MEMORY_BASE_URL  (default http://127.0.0.1:11435)
  FUSION_MEMORY_API_KEY   (required, fm-server Bearer auth, B5)
"""

from __future__ import annotations

import logging
import os
import time
from typing import Any

import httpx

logger = logging.getLogger(__name__)

DEFAULT_BASE_URL = "http://127.0.0.1:11435"


class _HttpBase:
    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        timeout: float = 10.0,
    ) -> None:
        self._base_url = (
            base_url or os.environ.get("FUSION_MEMORY_BASE_URL") or DEFAULT_BASE_URL
        ).rstrip("/")
        key = api_key or os.environ.get("FUSION_MEMORY_API_KEY")
        if not key:
            raise RuntimeError("FUSION_MEMORY_API_KEY not set (fm-server Bearer auth, B5)")
        self._client = httpx.Client(
            base_url=self._base_url,
            headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
            timeout=timeout,
        )

    def _rpc(self, method: str, params: dict[str, Any]) -> Any:
        resp = self._client.post(
            f"/v1/memory/{method}",
            json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1},
        )
        resp.raise_for_status()
        data = resp.json()
        if data.get("error"):
            err = data["error"]
            raise RuntimeError(f"fusion-memory {method} RPC {err['code']}: {err['message']}")
        return data.get("result")

    def close(self) -> None:
        self._client.close()


class HarnessMemorySink(_HttpBase):
    """MemorySink adapter: store a compaction summary as one Episodic memory.

    key is the harness scope:id pair ("compaction:<session_id>"); session_id
    falls back to the part after the first ':' so unscoped keys still land.
    """

    def store(
        self, key: str, payload: str, metadata: dict[str, Any] | None = None
    ) -> None:
        meta = dict(metadata or {})
        session_id = str(meta.get("session_id") or key.split(":", 1)[-1])
        meta.setdefault("source", "harness-sink")
        meta.setdefault("key", key)
        interaction = {
            "id": key,
            "session_id": session_id,
            "turns": [
                {
                    "turn_idx": 0,
                    "user_message": payload,
                    "assistant_message": "",
                    "tool_calls": [],
                }
            ],
            "timestamp": int(time.time() * 1000),
            "metadata": meta,
        }
        ids = self._rpc(
            "commit", {"session_id": session_id, "interaction": interaction}
        )
        logger.info(
            "harness sink stored key=%s session=%s turn_ids=%s", key, session_id, ids
        )


class HarnessMemorySource(_HttpBase):
    """MemorySource adapter: just-in-time recall over the retrieve RPC.

    Returns dicts shaped for MemoryBridge.inject_context (payload key).
    Network outages degrade to [] (bounded no-op); RPC-level errors still
    surface — MemoryBridge.recall catches them and logs.
    """

    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        timeout: float = 10.0,
        token_budget: int = 2048,
        aggregate: bool = True,
        namespace: str = "",
        namespace_key: str = "",
    ) -> None:
        super().__init__(base_url=base_url, api_key=api_key, timeout=timeout)
        self._token_budget = token_budget
        self._aggregate = aggregate
        self._namespace = namespace
        self._namespace_key = namespace_key

    def retrieve(self, query: str, limit: int = 5) -> list[dict[str, Any]]:
        if not query:
            return []
        params: dict[str, Any] = {
            "text": query,
            "top_k": max(1, int(limit)),
            "token_budget": self._token_budget,
            "aggregate": self._aggregate,
        }
        if self._namespace:
            params["namespace"] = self._namespace
        if self._namespace_key:
            params["namespace_key"] = self._namespace_key
        try:
            result = self._rpc("retrieve", params)
        except httpx.HTTPError as exc:
            logger.warning("harness source unavailable, degrade to empty: %s", exc)
            return []
        blocks = (result or {}).get("blocks") or []
        items = [
            {
                "payload": b.get("turns_text") or "",
                "interaction_id": b.get("interaction_id"),
                "score": b.get("score"),
            }
            for b in blocks
        ]
        logger.info("harness source recalled query_chars=%d items=%d", len(query), len(items))
        return items
