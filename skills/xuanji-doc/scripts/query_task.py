#!/usr/bin/env python3
"""查询璇玑Agent 文档提取任务状态。通过 yumclaw Gateway API 发送 CMD 查询消息。"""

import os
import sys
import json
import argparse

import requests

GATEWAY_URL = os.environ.get("GATEWAY_URL", "http://127.0.0.1:42617")
GATEWAY_TOKEN = os.environ.get("GATEWAY_TOKEN", "")
XUANJI_WK_UID = os.environ.get("XUANJI_WK_UID", "xuanji_agent")


def send_cmd_via_gateway(recipient: str, cmd: str, param: dict) -> dict:
    payload = {
        "recipient": recipient,
        "channel_type": 1,
        "message": {"type": 99, "cmd": cmd, "param": param},
    }
    headers = {"Content-Type": "application/json"}
    if GATEWAY_TOKEN:
        headers["Authorization"] = f"Bearer {GATEWAY_TOKEN}"

    resp = requests.post(
        f"{GATEWAY_URL}/v1/channels/wukongim/send",
        json=payload,
        headers=headers,
        timeout=10,
    )
    resp.raise_for_status()
    return resp.json()


def main():
    parser = argparse.ArgumentParser(description="查询璇玑Agent 提取任务状态")
    parser.add_argument("--execution_id", required=True, help="任务 execution_id")
    args = parser.parse_args()

    try:
        result = send_cmd_via_gateway(
            XUANJI_WK_UID, "xuanji.query_extraction_task",
            {"execution_id": args.execution_id},
        )
    except requests.exceptions.ConnectionError:
        print(json.dumps({
            "success": False,
            "output": "yumclaw Gateway 未运行，无法查询任务状态",
        }))
        sys.exit(1)
    except requests.exceptions.RequestException as e:
        print(json.dumps({
            "success": False,
            "output": f"查询失败: {e}",
        }))
        sys.exit(1)

    status = result.get("status", "unknown")
    if status == "completed":
        output = (
            f"任务 {args.execution_id} 已完成\n"
            + json.dumps(result.get("files", []), ensure_ascii=False, indent=2)
        )
    elif status == "failed":
        output = f"任务 {args.execution_id} 失败：{result.get('error', '未知错误')}"
    elif status in ("pending", "running"):
        progress = result.get("progress", 0)
        output = f"任务 {args.execution_id} 状态：{status}，进度：{progress}%"
    else:
        output = f"任务 {args.execution_id} 状态：{status}"

    print(json.dumps({"success": True, "output": output}))


if __name__ == "__main__":
    main()
