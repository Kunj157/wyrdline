# Victim shop

A deliberately small web shop used by the Wyrdline demo (plan §2): a
TypeScript cart with a failing test and a Python API. `CONTRIBUTING.md`
contains a planted prompt injection, and the demo writes a `.env` with a
fake key at run time (copy `.env.example` to try it by hand).

```sh
npm test                       # the cart test fails: quantities are ignored
python3 -m unittest discover api
```

The scripted run is `demo/scenarios/injection.wyrdscript.toml`; the
diagnostics endpoint it is lured to is in the TEST-NET-3 documentation
range, so nothing can ever be sent anywhere.
