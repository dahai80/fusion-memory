"""fusion-memory reference HTTP client (fusion-cowork / fusion-agent-studio HTTP path).

PRD §10.1 / §10.3。PyO3 嵌入 (fm-py) 是首选进程内路径; HTTP 走 fm-server。
本客户端供消费方在不能用 PyO3 (build 环境无 Rust) 或跨进程隔离时走 HTTP。

安装: 复制本文件到消费方, 依赖 httpx (两消费方 pyproject 均已有 httpx>=0.27)。
env (operator 配置):
  FUSION_MEMORY_BASE_URL  (默认 http://127.0.0.1:11435)
  FUSION_MEMORY_API_KEY   (必配, 对齐 fm-server Bearer B5)

wire: JSON-RPC 2.0 envelope。同 clients/ts/fusionMemoryClient.ts。
"""

from __future__ import annotations

import os
from typing import Any

import httpx

DEFAULT_BASE_URL = "http://127.0.0.1:11435"


class FusionMemoryClient:
    """fusion-memory HTTP 客户端。httpx.AsyncClient + Bearer 鉴权 (B5)。"""

    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        timeout: float = 10.0,
    ) -> None:
        self._base_url = (base_url or os.environ.get("FUSION_MEMORY_BASE_URL") or DEFAULT_BASE_URL).rstrip("/")
        key = api_key or os.environ.get("FUSION_MEMORY_API_KEY")
        if not key:
            raise RuntimeError("FUSION_MEMORY_API_KEY 未配置 (fm-server Bearer 鉴权, B5)")
        self._client = httpx.AsyncClient(
            base_url=self._base_url,
            headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
            timeout=timeout,
        )

    async def _rpc(self, method: str, params: dict[str, Any]) -> Any:
        resp = await self._client.post(
            f"/v1/memory/{method}",
            json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1},
        )
        resp.raise_for_status()
        data = resp.json()
        if data.get("error"):
            err = data["error"]
            raise RuntimeError(f"fusion-memory {method} RPC {err['code']}: {err['message']}")
        return data.get("result")

    async def commit_episodic_memory(self, session_id: str, interaction: dict[str, Any]) -> list[str]:
        """写入 Interaction, 返回 turn 级 memory_id 列表。"""
        return await self._rpc("commit", {"session_id": session_id, "interaction": interaction})

    async def retrieve_context(
        self,
        text: str,
        top_k: int = 10,
        token_budget: int = 4096,
        aggregate: bool = True,
    ) -> dict[str, Any]:
        """检索记忆上下文, 返回 {blocks, total_tokens}。"""
        return await self._rpc(
            "retrieve",
            {"text": text, "top_k": top_k, "token_budget": token_budget, "aggregate": aggregate},
        )

    async def consolidate_memories(self) -> dict[str, Any]:
        """触发遗忘/合并 saga, 返回报告。"""
        return await self._rpc("consolidate", {})

    async def get_memory(self, memory_id: str) -> dict[str, Any] | None:
        """按 id 取记忆 (miss → None)。"""
        return await self._rpc("get", {"id": memory_id})

    async def delete_memory(self, memory_id: str) -> str:
        """软删 (需 confirm=true, B5 二次确认)。"""
        return await self._rpc("delete", {"id": memory_id, "confirm": True})

    async def check_health(self) -> bool:
        try:
            resp = await self._client.get("/healthz", timeout=2.0)
            return resp.is_success
        except Exception:
            return False

    async def close(self) -> None:
        await self._client.aclose()
