"""Tests for harness_bridge.py (issue #42).

Run: pytest clients/python/test_harness_bridge.py -v
E2E cases hit a real fm-server (FM_TEST_BASE_URL + FM_TEST_API_KEY env, or
127.0.0.1:11435 + FM_HOME/api_key default); skipped when the server is down.
Protocol conformance cases are offline (runtime_checkable Protocol via the
real fusion-harness tree when available, else skipped).
"""

from __future__ import annotations

import os
import pathlib
import sys
import uuid

import httpx
import pytest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from harness_bridge import HarnessMemorySink, HarnessMemorySource  # noqa: E402

HERE = pathlib.Path(__file__).parent


def _test_endpoint():
    base = os.environ.get("FM_TEST_BASE_URL") or "http://127.0.0.1:11435"
    key = os.environ.get("FM_TEST_API_KEY")
    if key is None:
        key_file = pathlib.Path.home() / ".fusion-memory" / "api_key"
        if key_file.exists():
            key = key_file.read_text().strip()
    if not key:
        return None
    return base, key


def _server_up(base: str, key: str) -> bool:
    try:
        return httpx.get(f"{base}/healthz", timeout=2.0).is_success
    except Exception:
        return False


def _test_harness_import():
    for p in os.environ.get("FM_TEST_HARNESS_PATH", "").split(":"):
        if p and (pathlib.Path(p) / "fusion_harness" / "memory_link.py").exists():
            return p
    guess = pathlib.Path.home() / "fusion" / "fusion-harness"
    if (guess / "fusion_harness" / "memory_link.py").exists():
        return str(guess)
    return None


HARNESS = _test_harness_import()


def test_sink_protocol_conformance():
    if not HARNESS:
        pytest.skip("fusion-harness tree not available")
    sys.path.insert(0, HARNESS)
    from fusion_harness.memory_link import MemoryBridge, MemorySink  # noqa: E402

    ep = _test_endpoint()
    sink = HarnessMemorySink(*ep) if ep else HarnessMemorySink(
        base_url="http://127.0.0.1:1", api_key="unused"
    )
    assert isinstance(sink, MemorySink)
    bridge = MemoryBridge(sink=sink)
    assert callable(getattr(sink, "store", None))
    assert hasattr(bridge, "persist_compaction")


def test_source_protocol_conformance():
    if not HARNESS:
        pytest.skip("fusion-harness tree not available")
    sys.path.insert(0, HARNESS)
    from fusion_harness.memory_link import MemoryBridge, MemorySource  # noqa: E402

    ep = _test_endpoint()
    src = HarnessMemorySource(*ep) if ep else HarnessMemorySource(
        base_url="http://127.0.0.1:1", api_key="unused"
    )
    assert isinstance(src, MemorySource)
    bridge = MemoryBridge(source=src)
    assert hasattr(bridge, "recall") and hasattr(bridge, "inject_context")


def test_missing_api_key_raises():
    env = {
        k: v
        for k, v in os.environ.items()
        if k != "FUSION_MEMORY_API_KEY"
    }
    import subprocess

    code = (
        "import sys\n"
        f"sys.path.insert(0, {str(HERE)!r})\n"
        "import os\n"
        "os.environ.pop('FUSION_MEMORY_API_KEY', None)\n"
        "try:\n"
        "    from harness_bridge import HarnessMemorySink\n"
        "    HarnessMemorySink()\n"
        "except RuntimeError as e:\n"
        "    assert 'API_KEY' in str(e)\n"
        "    print('ok')\n"
        "else:\n"
        "    raise SystemExit('expected RuntimeError')\n"
    )
    r = subprocess.run(
        [sys.executable, "-I", "-c", code], env=env, capture_output=True, text=True
    )
    assert "ok" in r.stdout, f"expected clean RuntimeError, got {r.stderr}"


@pytest.fixture(scope="module")
def endpoint():
    ep = _test_endpoint()
    if ep is None or not _server_up(*ep):
        pytest.skip("fm-server not running (start with ./start.sh start)")
    return ep


@pytest.fixture(scope="module")
def session_id():
    return f"harness-e2e-{uuid.uuid4().hex[:8]}"


def test_sink_store_and_source_recall_e2e(endpoint, session_id):
    base, key = endpoint
    sink = HarnessMemorySink(base_url=base, api_key=key)
    marker = f"UNIQUE-MARKER-{session_id} deploy runbook: rollback via fusion-sv restart"
    sink.store(
        key=f"compaction:{session_id}",
        payload=marker,
        metadata={"scope": "compaction", "session_id": session_id},
    )

    src = HarnessMemorySource(base_url=base, api_key=key)
    hits = src.retrieve(f"{session_id} rollback runbook", limit=5)
    assert isinstance(hits, list)
    payloads = " ".join(str(h.get("payload", "")) for h in hits)
    assert marker in payloads, f"stored summary not recalled; got {hits}"


def test_bridge_roundtrip_with_real_harness(endpoint, session_id):
    if not HARNESS:
        pytest.skip("fusion-harness tree not available")
    sys.path.insert(0, HARNESS)
    from fusion_harness.memory_link import MemoryBridge  # noqa: E402

    base, key = endpoint
    bridge = MemoryBridge(
        sink=HarnessMemorySink(base_url=base, api_key=key),
        source=HarnessMemorySource(base_url=base, api_key=key),
    )
    summary = f"COMPACTION-SUMMARY-{session_id} session covered auth refactor"
    assert bridge.persist_compaction(summary, session_id=session_id) is True

    recalled = bridge.recall(f"{session_id} auth refactor", limit=3)
    assert any("COMPACTION-SUMMARY" in str(i.get("payload")) for i in recalled)

    messages = [
        {"role": "system", "content": "base"},
        {"role": "user", "content": "continue"},
    ]
    out = bridge.inject_context(messages, f"{session_id} auth refactor", limit=2)
    assert len(out) == 3
    assert out[1]["role"] == "system"
    assert "COMPACTION-SUMMARY" in out[1]["content"]


def test_source_bounded_limit(endpoint, session_id):
    base, key = endpoint
    sink = HarnessMemorySink(base_url=base, api_key=key)
    for i in range(3):
        sink.store(
            key=f"compaction:{session_id}-lim-{i}",
            payload=f"LIMIT-CASE-{session_id} item {i}",
            metadata={"session_id": session_id},
        )
    src = HarnessMemorySource(base_url=base, api_key=key)
    hits = src.retrieve(f"LIMIT-CASE-{session_id}", limit=2)
    assert len(hits) <= 2


def test_source_outage_degrades_to_empty():
    src = HarnessMemorySource(base_url="http://127.0.0.1:9", api_key="unused", timeout=1.0)
    assert src.retrieve("anything", limit=3) == []
