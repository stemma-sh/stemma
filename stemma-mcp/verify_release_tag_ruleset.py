#!/usr/bin/env python3
"""Verify the repository ruleset that protects release tags from mutation.

Usage:
    python3 stemma-mcp/verify_release_tag_ruleset.py select rulesets.json
    python3 stemma-mcp/verify_release_tag_ruleset.py verify ruleset.json 12345

``select`` prints the unique active ``protected-release-tags`` ruleset ID.
``verify`` validates the corresponding detail response and prints a compact
JSON receipt. Both commands are read-only.
"""

import argparse
import json
import re
import sys
from pathlib import Path


RULESET_NAME = "protected-release-tags"
RELEASE_TAG_PATTERN = "refs/tags/v*"
MAX_JSON_BYTES = 5 * 1024 * 1024
POSITIVE_INTEGER_RE = re.compile(r"[1-9][0-9]*")


class RulesetError(Exception):
    """The API response does not prove the required release-tag protection."""


class DuplicateJsonKey(ValueError):
    """A JSON object contains an ambiguous duplicate member name."""


def require(condition, message):
    if not condition:
        raise RulesetError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonKey("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result


def reject_nonfinite(value):
    raise ValueError("non-finite JSON number {}".format(value))


def strict_read_json(path):
    require(path.exists(), "JSON input does not exist: {}".format(path))
    require(not path.is_symlink(), "JSON input must not be a symlink: {}".format(path))
    require(path.is_file(), "JSON input must be a regular file: {}".format(path))
    size = path.stat().st_size
    require(size > 0, "JSON input is empty: {}".format(path))
    require(
        size <= MAX_JSON_BYTES,
        "JSON input exceeds {} bytes: {}".format(MAX_JSON_BYTES, path),
    )
    content = path.read_bytes()
    require(
        len(content) <= MAX_JSON_BYTES,
        "JSON input grew beyond {} bytes while reading: {}".format(
            MAX_JSON_BYTES, path
        ),
    )
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RulesetError("JSON input is not UTF-8: {}".format(error))
    try:
        return json.loads(
            text,
            object_pairs_hook=unique_object,
            parse_constant=reject_nonfinite,
        )
    except (ValueError, json.JSONDecodeError, DuplicateJsonKey) as error:
        raise RulesetError("input is not strict JSON: {}".format(error))


def require_ruleset_id(value, field):
    require(
        type(value) is int and value > 0,
        "{} must be a positive integer".format(field),
    )
    return value


def select_ruleset(payload):
    require(isinstance(payload, list), "ruleset list response must be a JSON array")
    matches = []
    for index, ruleset in enumerate(payload):
        require(
            isinstance(ruleset, dict),
            "ruleset list entry {} must be an object".format(index),
        )
        if (
            ruleset.get("name") == RULESET_NAME
            and ruleset.get("enforcement") == "active"
        ):
            matches.append(
                require_ruleset_id(
                    ruleset.get("id"), "ruleset list entry {} id".format(index)
                )
            )
    require(
        len(matches) == 1,
        "expected exactly one active ruleset named {!r}; found {}".format(
            RULESET_NAME, len(matches)
        ),
    )
    return matches[0]


def parse_requested_id(value):
    require(
        POSITIVE_INTEGER_RE.fullmatch(value) is not None,
        "ruleset ID argument must be a positive integer",
    )
    return int(value)


def verify_ruleset(payload, requested_id):
    require(isinstance(payload, dict), "ruleset detail response must be a JSON object")
    require(
        require_ruleset_id(payload.get("id"), "ruleset detail id") == requested_id,
        "ruleset detail id does not match selected id {}".format(requested_id),
    )
    require(
        payload.get("name") == RULESET_NAME,
        "ruleset name is not {!r}".format(RULESET_NAME),
    )
    require(payload.get("target") == "tag", "ruleset target must be 'tag'")
    require(
        payload.get("enforcement") == "active",
        "ruleset enforcement must be 'active'",
    )

    conditions = payload.get("conditions")
    require(isinstance(conditions, dict), "ruleset conditions must be an object")
    ref_name = conditions.get("ref_name")
    require(isinstance(ref_name, dict), "ruleset conditions.ref_name must be an object")
    require(
        ref_name.get("include") == [RELEASE_TAG_PATTERN],
        "ruleset ref include must be exactly {!r}".format(
            [RELEASE_TAG_PATTERN]
        ),
    )
    require(
        ref_name.get("exclude") == [],
        "ruleset ref exclude must be an empty array",
    )

    rules = payload.get("rules")
    require(isinstance(rules, list), "ruleset rules must be an array")
    require(
        all(isinstance(rule, dict) for rule in rules),
        "every ruleset rule must be an object",
    )
    rule_types = [rule.get("type") for rule in rules]
    require(
        all(isinstance(rule_type, str) for rule_type in rule_types),
        "every ruleset rule type must be a string",
    )
    require(
        len(rule_types) == 2 and set(rule_types) == {"update", "deletion"},
        "ruleset rule types must be exactly update and deletion",
    )

    bypass_visible = "bypass_actors" in payload
    if bypass_visible:
        require(
            payload["bypass_actors"] == [],
            "visible bypass_actors must be an empty array",
        )
    return {
        "bypass_actors_visible": bypass_visible,
        "enforcement": "active",
        "id": requested_id,
        "name": RULESET_NAME,
        "ref_include": [RELEASE_TAG_PATTERN],
        "rule_types": ["deletion", "update"],
        "target": "tag",
        "verified": True,
    }


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    select = subparsers.add_parser("select", help="select the unique active ruleset")
    select.add_argument("list_json", help="ruleset-list API response")
    verify = subparsers.add_parser("verify", help="verify one ruleset detail response")
    verify.add_argument("detail_json", help="ruleset-detail API response")
    verify.add_argument("id", help="ruleset id returned by select")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    try:
        if args.command == "select":
            ruleset_id = select_ruleset(strict_read_json(Path(args.list_json)))
            print(ruleset_id)
        else:
            requested_id = parse_requested_id(args.id)
            receipt = verify_ruleset(
                strict_read_json(Path(args.detail_json)), requested_id
            )
            print(json.dumps(receipt, sort_keys=True))
    except (RulesetError, OSError) as error:
        print("error: {}".format(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
