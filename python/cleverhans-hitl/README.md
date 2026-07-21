# cleverhans-hitl

Python host binding for [CleverHans](https://github.com/nordalf/cleverhans) —
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
```

The validation pipeline, state machine, and agent loop run in the shared Rust
core (`cleverhans-core`); this package is a thin asyncio-native binding over
it. Handlers and the LLM callable may be sync or `async def`.

See the repository for the protocol spec, conformance vectors, and
integration guides.
