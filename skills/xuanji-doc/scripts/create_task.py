#!/usr/bin/env python3
"""向璇玑Agent 提交文档内容提取任务。通过 yumclaw Gateway API 发送 CMD 消息。"""

import os
import sys
import json
import argparse

import requests

GATEWAY_URL = os.environ.get("GATEWAY_URL", "http://127.0.0.1:42617")
GATEWAY_TOKEN = os.environ.get("GATEWAY_TOKEN", "")
XUANJI_WK_UID = os.environ.get("XUANJI_WK_UID", "xuanji_agent")


def send_cmd_via_gateway(recipient: str, cmd: str, param: dict) -> dict:
    """通过 yumclaw Gateway API 发送 CMD 消息到 WuKongIM"""
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
    parser = argparse.ArgumentParser(description="提交文档提取任务到璇玑Agent")
    parser.add_argument("--user_text", required=True, help="用户发送文件时的原始文字")
    parser.add_argument("--files", required=True, help="文件列表 JSON 数组")
    parser.add_argument("--la_id", required=True, help="yumclaw 实例 la_id")
    args = parser.parse_args()

    files = json.loads(args.files)
    if not isinstance(files, list) or len(files) == 0:
        print(json.dumps({"success": False, "output": "files 参数不能为空"}))
        sys.exit(1)

    param = {
        "sid": args.la_id,
        "reply_to": args.la_id,
        "user_text": args.user_text,
        "files": files,
    }

    try:
        result = send_cmd_via_gateway(
            XUANJI_WK_UID, "xuanji.create_extraction_task", param
        )
    except requests.exceptions.ConnectionError:
        print(json.dumps({
            "success": False,
            "output": "yumclaw Gateway 未运行，无法发送消息到璇玑Agent",
        }))
        sys.exit(1)
    except requests.exceptions.Timeout:
        print(json.dumps({
            "success": False,
            "output": "璇玑Agent 响应超时，请稍后重试",
        }))
        sys.exit(1)
    except requests.exceptions.RequestException as e:
        print(json.dumps({
            "success": False,
            "output": f"发送消息失败: {e}",
        }))
        sys.exit(1)

    execution_id = result.get("execution_id", "unknown")
    print(json.dumps({
        "success": True,
        "output": (
            f"已提交 {len(files)} 个文件的提取任务\n"
            f"execution_id: {execution_id}\n"
            f"预计 30-60 秒完成，完成后会主动通知您"
        ),
        "execution_id": execution_id,
    }))


if __name__ == "__main__":
    main()
