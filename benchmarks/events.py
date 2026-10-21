"""Allowlisted provider projections; private envelopes never leave .scratch."""

import json
import re


def sanitize(value, aliases):
    if isinstance(value, dict):
        return {key: sanitize(item, aliases) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize(item, aliases) for item in value]
    if not isinstance(value, str):
        return value
    for source, replacement in sorted(aliases.items(), key=lambda pair: -len(pair[0])):
        value = value.replace(source, replacement)
    if re.search(r"/(?:Users|home|private|tmp|var/folders)/|sk-[A-Za-z0-9_-]{16,}", value):
        raise ValueError("Unsafe path or credential shape in public projection")
    return value


def project(provider, raw, aliases):
    events = [json.loads(line) for line in raw.splitlines() if line.startswith("{")]
    public, usage, models = [], [], []
    for event in events:
        kind = event.get("type")
        if provider == "codex":
            if kind == "turn.completed":
                usage.append(event["usage"])
            if kind == "item.completed":
                item = event["item"]
                if item["type"] == "command_execution":
                    public.append({key: item[key] for key in
                                   ("type", "command", "aggregated_output", "exit_code")})
                elif item["type"] == "agent_message":
                    public.append({"type": "answer", "text": item["text"]})
            if kind in ("error", "turn.failed"):
                public.append({"type": "error", "message": event.get("message", "turn failed")})
        else:
            if kind == "system" and event.get("subtype") == "init":
                models.append(event.get("model"))
            if kind == "assistant":
                for block in event.get("message", {}).get("content", []):
                    if block.get("type") in ("text", "tool_use"):
                        public.append({key: value for key, value in block.items() if key != "id"})
            if kind == "user":
                for block in event.get("message", {}).get("content", []):
                    if block.get("type") == "tool_result":
                        public.append({key: block[key] for key in ("type", "content", "is_error") if key in block})
            if kind == "result":
                usage.append(event.get("usage", {}))
                public.append({key: event[key] for key in
                               ("type", "subtype", "result", "is_error", "total_cost_usd", "modelUsage") if key in event})
    return sanitize({"events": public, "usage": usage, "models": models}, aliases)
