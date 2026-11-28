# cleverhans-hitl

Python host binding for [CleverHans](https://github.com/2commits/cleverhans) —
a propose-only, in-app, human-in-the-loop agent framework. The agent never
acts on your system: it proposes actions from your closed registry; your
handlers execute only after explicit user confirmation, under the principal
you bound to the session.

```python
import cleverhans_agent

agent = cleverhans_agent.Agent(
    registry="registry.json",             # path, JSON string, or dict
    handlers={"doc.publish": publish},    # async (params, principal)
    dry_runs={"doc.publish": publish_dry_run},
    authorize=authorize,                  # None/True allow, str deny
    llm={"provider": "anthropic", "api_key": "..."},
)
session = agent.session({"user_id": "u-1", "roles": ["editor"]})
async for event in session.handle(frame):  # dicts, one per ServerEvent
    await ws.send_json(event)
if session.closed:  # a non-recoverable error ended the session (spec §6.3)
    await ws.close(code=session.close_code, reason=session.close_reason)
```

Your WebSocket server answers the upgrade, so it also picks the subprotocol:
accept with `subprotocol=cleverhans_agent.SUBPROTOCOL` (`"cleverhans.v0.1"`)
when the client offers it — `@cleverhans/react` offers it first whenever it
is given `protocols` — and with none otherwise. `python/examples/fastapi_ws.py`
shows both.

The validation pipeline, state machine, and agent loop run in the shared Rust
core (`cleverhans-core`); this package is a thin asyncio-native binding over
it. Handlers and the LLM callable may be sync or `async def`.

See the repository for the protocol spec, conformance vectors, and
integration guides.
