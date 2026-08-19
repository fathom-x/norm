<h1 align="center">norm</h1>
<p align="center">Your Overpay companion.</p>

---

Norm is your Overpay companion — it talks to Overpay and can make purchases on your behalf via [owallet](owallet/) (bundled with norm).

Norm is a fork of [opencode](https://opencode.ai), the open source AI coding agent, so everything opencode can do, norm can do too.

### Installation

```bash
curl -fsSL https://raw.githubusercontent.com/fathom-x/norm/main/install | bash
```

> [!NOTE]
> While this repo is private, both the script fetch and the release download need a token with repo read access:
>
> ```bash
> curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
>   https://raw.githubusercontent.com/fathom-x/norm/main/install \
>   | GITHUB_TOKEN=$GITHUB_TOKEN bash
> ```

### Getting started

After norm is installed, initialize owallet:

```bash
owallet init
```

That will set up your `OWALLET_PASSWORD`, which is used to unlock and authorize funds.

Then create a spending account:

```bash
owallet generate
```

That generates a seed phrase which you **must write down!**

Now you're ready to use norm:

```bash
export OWALLET_PASSWORD=...

norm
```

### Agents

Norm includes two built-in agents you can switch between with the `Tab` key.

- **build** - Default, full-access agent for development work
- **plan** - Read-only agent for analysis and code exploration
  - Denies file edits by default
  - Asks permission before running bash commands
  - Ideal for exploring unfamiliar codebases or planning changes

Also included is a **general** subagent for complex searches and multistep tasks.
This is used internally and can be invoked using `@general` in messages.

### Documentation

Norm keeps opencode's configuration surface (`opencode.json`, `.opencode/` dirs, `OPENCODE_*` env vars), so the [opencode docs](https://opencode.ai/docs) apply. For the norm layer itself — the Overpay provider, the owallet MCP server, and its env knobs — see [CLAUDE.md](CLAUDE.md) and [owallet/README.md](owallet/).

### Contributing

If you're interested in contributing, please read the [contributing docs](./CONTRIBUTING.md) before submitting a pull request.
