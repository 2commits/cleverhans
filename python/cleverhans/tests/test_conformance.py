"""Runs every agent/binding conformance vector through the Python binding,
with all seams (handlers, dry-runs, slots, authz) implemented as Python
callables — the bridge itself is under test."""

from __future__ import annotations

import asyncio

import pytest
from vector_runner import load_dir, run_vector

FIXTURES = {fixture["name"]: fixture for fixture in load_dir("fixtures")}
VECTORS = load_dir("cases")


@pytest.mark.parametrize("vector", VECTORS, ids=[vector["name"] for vector in VECTORS])
def test_vector(vector):
    fixture = FIXTURES[vector["fixture"]]
    asyncio.run(run_vector(fixture, vector))
