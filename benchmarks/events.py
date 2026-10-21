"""Allowlisted provider projections; private envelopes never leave .scratch."""

import json
import re


CREDENTIAL = re.compile(
    r"sk-[A-Za-z0-9_-]{16,}|(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{20,}|"
    r"glpat-[A-Za-z0-9_-]{16,}|(?:AKIA|ASIA)[A-Z0-9]{16}|"
    r"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----|"
    r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}|"
    r"authorization\s*[:=]\s*['\"]?\s*(?:bearer|basic)\s+[^\s'\"]+|"
    r"(?:api[_-]?key|access[_-]?token|auth[_-]?token|secret|password|"
    r"gh_token|gl_token|github_token|gitlab_token|anthropic_api_key|openai_api_key|aws_secret_access_key|aws_session_token)\s*['\"]?\s*[:=]\s*['\"]?[^\s'\",}]+|"
    r"https?://[^\s/:@]+:[^\s/@]+@",
    re.IGNORECASE,
)


def final_text(public):
    answers = [event.get("text", "") for event in public["events"]
               if isinstance(event, dict) and event.get("type") == "answer"]
    if answers:
        return answers[-1]
    results = [event.get("result", "") for event in public["events"]
               if isinstance(event, dict) and event.get("type") == "result"]
    return results[-1] if results else ""


def sanitize(value, aliases):
    if isinstance(value, dict):
        return {key: sanitize(item, aliases) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize(item, aliases) for item in value]
    if not isinstance(value, str):
        return value
    for source, replacement in sorted(aliases.items(), key=lambda pair: -len(pair[0])):
        value = value.replace(source, replacement)
    value = re.sub(r"/var/folders/[^'\"\s\\]+", "<runtime-temp>", value)
    value = re.sub(r"/tmp/cc-socks/[^'\"\s\\]+", "<runtime-socket>", value)
    if re.search(r"/(?:Users|home|private|tmp|var/folders)/", value) or CREDENTIAL.search(value):
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
