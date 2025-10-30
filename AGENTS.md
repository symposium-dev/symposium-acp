This repository contains the Symposium implementation of the Agent Client Protocol.

The mdbook Table of Contents in `md/SUMMARY.md` contains all relevant documentation. You should keep it up to date as you work.

The crates are found in `src/*` directory. The `src/README.md` file provides an overview of the crates and their respective functionalities.

## Conventional Commits

This repository uses **Conventional Commits** for automated releases via release-plz. All commit messages should follow this format:

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

### Commit Types

- **feat:** A new feature (triggers minor version bump)
- **fix:** A bug fix (triggers patch version bump)
- **docs:** Documentation only changes
- **style:** Code style changes (formatting, missing semicolons, etc.)
- **refactor:** Code changes that neither fix bugs nor add features
- **perf:** Performance improvements
- **test:** Adding or updating tests
- **chore:** Maintenance tasks, dependency updates, etc.
- **ci:** CI/CD configuration changes
- **build:** Build system or external dependency changes

### Breaking Changes

Add `!` after the type to indicate breaking changes (triggers major version bump):
```
feat!: change API to use async traits
```

Or include `BREAKING CHANGE:` in the footer:
```
feat: redesign conductor protocol

BREAKING CHANGE: conductor now requires explicit capability registration
```

### Examples

```
feat(conductor): add support for dynamic proxy chains
fix(sacp): resolve deadlock in message routing
docs: update README with installation instructions
chore: bump tokio to 1.40
```

### Scope Guidelines

Common scopes for this repository:
- `sacp` - Core protocol changes
- `sacp-proxy` - Proxy framework changes
- `conductor` - Conductor-specific changes
- `deps` - Dependency updates

**Agents should help ensure commit messages follow this format.**
