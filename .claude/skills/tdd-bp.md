# Test-Driven Development: Complete Practitioner Reference

TDD reduces defects by **40-90%** while increasing initial development time by 15-35%—a tradeoff that pays off through dramatically lower maintenance costs. The Red-Green-Refactor cycle forces interface-first design, naturally producing SOLID-compliant, loosely coupled code. This guide provides actionable patterns, tool recommendations, and evidence-based practices for immediate application.

---

## TDD cycle mechanics

```
┌─────────────────────────────────────────────────────┐
│  0. WRITE TEST LIST (all expected behaviors)        │
└────────────────────────┬────────────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  1. RED: Write ONE failing    │◄────────┐
         │     test (~30 seconds)        │         │
         └───────────────┬───────────────┘         │
                         ▼                         │
         ┌───────────────────────────────┐         │
         │  2. GREEN: Minimal code to    │         │
         │     pass (even hard-coded)    │         │
         └───────────────┬───────────────┘         │
                         ▼                         │
         ┌───────────────────────────────┐         │
         │  3. REFACTOR: Clean up,       │         │
         │     eliminate duplication     │─────────┘
         └───────────────────────────────┘
              Target: 20-40 cycles/hour
```

### Core workflow steps

| Step | Action | Duration | Key Rule |
|------|--------|----------|----------|
| 0 | List all behavior variants to test | Pre-work | No implementation decisions |
| 1 | Write ONE failing test with assertion | ~30 sec | Work backwards from assertions |
| 2 | Write minimum code to pass | Minutes | Hard-coded values OK |
| 3 | Refactor (optional) | Variable | Run tests after each change |
| 4 | Commit, pick next test | — | RGRC: Red-Green-Refactor-Commit |

---

## Testing patterns table

| Pattern | Structure | When to Use |
|---------|-----------|-------------|
| **AAA (Arrange-Act-Assert)** | Setup → Execute → Verify | All unit tests; industry standard |
| **Given-When-Then** | Precondition → Action → Outcome | BDD acceptance tests; stakeholder communication |
| **Table-Driven** | Data table with inputs/expected outputs | Same logic, multiple inputs |
| **Test Data Builder** | Fluent builder with defaults | Complex object construction |
| **Fresh Fixtures** | New setup for each test | Isolated, reproducible tests |

### Test doubles comparison

| Type | Purpose | Example Use |
|------|---------|-------------|
| **Dummy** | Fill parameters, never used | Empty object for constructor |
| **Stub** | Return canned answers | `when(repo.find(1)).thenReturn(user)` |
| **Fake** | Working shortcut implementation | In-memory database |
| **Spy** | Record calls while executing | Email service tracking sent messages |
| **Mock** | Verify interactions occurred | `verify(service).sendEmail()` |

### Code examples

```python
# AAA Pattern (pytest)
def test_calculate_discount():
    # Arrange
    cart = ShoppingCart()
    cart.add_item(Item(price=100))
    
    # Act
    total = cart.calculate_total(discount_percent=10)
    
    # Assert
    assert total == 90
```

```go
// Table-driven (Go)
func TestAdd(t *testing.T) {
    tests := []struct{ a, b, want int }{
        {1, 2, 3}, {-1, 1, 0}, {0, 0, 0},
    }
    for _, tc := range tests {
        got := Add(tc.a, tc.b)
        assert.Equal(t, tc.want, got)
    }
}
```

---

## Language framework reference

| Language | Test Framework | Mocking | Assertion Style |
|----------|---------------|---------|-----------------|
| **Python** | pytest | pytest-mock | `assert x == y` |
| **JavaScript/TS** | Vitest / Jest | Built-in | `expect(x).toBe(y)` |
| **Java** | JUnit 5 + Mockito | @Mock, when/verify | AssertJ: `assertThat(x).isEqualTo(y)` |
| **C#/.NET** | xUnit | Moq / NSubstitute | `Assert.Equal(expected, actual)` |
| **Go** | testing + testify | testify/mock | `assert.Equal(t, expected, actual)` |
| **Rust** | Built-in #[test] | mockall | `assert_eq!(x, y)` |

### Framework quick commands

```bash
# Python
pytest -v --cov=src tests/

# JavaScript (Vitest)
vitest run --coverage

# Java (Maven)
mvn test jacoco:report

# Go
go test -v -cover ./...

# Rust
cargo test
```

---

## Advanced techniques

### Coverage and mutation testing tools

| Category | Tool | Language | Key Feature |
|----------|------|----------|-------------|
| **Coverage** | Coverage.py | Python | Branch coverage, pytest integration |
| **Coverage** | Istanbul/nyc | JS/TS | Statement/branch/function metrics |
| **Coverage** | JaCoCo | Java | Maven/Gradle integration |
| **Mutation** | Stryker | JS/TS/.NET | 30+ mutation operators, dashboard |
| **Mutation** | PITest | Java | Fast bytecode mutation |
| **Mutation** | mutmut | Python | Simple CLI, pytest integration |

### Property-based testing

```python
# Hypothesis (Python)
from hypothesis import given, strategies as st

@given(st.lists(st.integers()))
def test_sort_preserves_length(xs):
    assert len(sorted(xs)) == len(xs)

@given(st.text())
def test_encode_decode_roundtrip(s):
    assert decode(encode(s)) == s
```

### Key advanced concepts

- **Mutation testing**: Measures test quality by injecting bugs; target **80%+ mutation score**
- **Property-based testing**: Generates hundreds of inputs automatically; catches edge cases
- **Contract testing**: Pact for microservices API verification without full integration
- **Async TDD**: Extract logic to synchronous functions; use Humble Object pattern

---

## Anti-patterns table

| Anti-Pattern | Problem | Solution |
|--------------|---------|----------|
| **The Inspector** | Tests know implementation details | Test behavior through public interfaces only |
| **Over-Mocking** | Testing mocks, not code | Mock only external boundaries (DB, network) |
| **Fragile Tests** | Break on unrelated changes | Decouple from implementation; test contracts |
| **The Giant** | Single test spans thousands of lines | One concept per test; split tests |
| **Slow Poke** | Tests run too slowly | Follow test pyramid; mock I/O; parallelize |
| **Generous Leftovers** | Tests share state | Fresh fixtures; clean up after each test |
| **Coverage Chasing** | Testing trivial code for metrics | Test meaningful behaviors; coverage is indicator only |

---

## Evidence summary

| Metric | Finding | Source |
|--------|---------|--------|
| **Defect reduction** | 40-90% fewer pre-release defects | Microsoft/IBM (2008) |
| **Development time** | 15-35% increase initially | Microsoft/IBM (2008) |
| **Debugging time** | Up to 90% reduction | IBM study |
| **Test coverage** | 79-88% block coverage | Siniaalto review |
| **Code quality** | 2.6-4.2x better by metrics | Siniaalto (2007) |
| **Time to proficiency** | ~1 year for team integration | Practitioner research |
| **Developer perception** | 92% believe TDD yields higher quality | Survey data |

### Research consensus
Strong evidence for defect reduction and maintainability improvement. Moderate evidence for short-term productivity decrease offset by long-term gains. Meta-analysis of 27 studies shows positive quality effects, especially in industrial settings.

---

## Decision framework

### Use TDD when

- Pure functions with clear inputs/outputs
- Well-understood business domains
- APIs, services, libraries, backend logic
- Complex business rules requiring documentation
- Team collaboration (tests communicate intent)
- New features in existing codebases

### Consider alternatives when

| Scenario | Alternative |
|----------|-------------|
| UI/visual outputs | Visual regression tools, E2E tests |
| Exploratory spikes | Spike first, then TDD the solution |
| External integrations | Contract testing, service virtualization |
| Legacy without tests | Characterization tests first |
| Throwaway prototypes | Skip TDD; add tests if promoting |

### TDD vs alternatives

| Approach | Focus | Test Level | Best For |
|----------|-------|------------|----------|
| **TDD** | Code correctness | Unit | Algorithms, APIs, backend |
| **BDD** | User behavior | Acceptance | Cross-team communication, business logic |
| **ATDD** | Requirements clarity | Acceptance | Customer validation, scope control |
| **Test-After** | Verification | Any | Spikes, learning, legacy characterization |

---

## Inside-out vs outside-in

| Aspect | Inside-Out (Detroit) | Outside-In (London) |
|--------|---------------------|---------------------|
| **Start from** | Domain/entities | UI/acceptance tests |
| **Test doubles** | Minimal | Heavy mocking |
| **Design emerges** | During refactor | During red phase |
| **Best for** | Well-understood domains | Vertical user stories |
| **Risk** | Parts may not integrate | Design baked into tests |

---

## Legacy code strategies

Apply Michael Feathers' approach from *Working Effectively with Legacy Code*:

1. **Write characterization tests** — Capture what code actually does
2. **Identify seams** — Points where behavior can be altered
3. **Break dependencies** — Extract interfaces, parameterize constructors
4. **Sprout/Wrap methods** — Add new tested code alongside legacy
5. **Apply TDD to all new code** — Expand coverage organically

---

## Common objections addressed

| Objection | Evidence-Based Response |
|-----------|------------------------|
| "TDD is too slow" | 40-90% fewer defects offset 15-35% time increase; reduced debugging |
| "Tests are hard to maintain" | Test behavior not implementation; refactor tests like production code |
| "Can't TDD everything" | Use Humble Object pattern for UI; contract tests for externals |
| "Codebase too legacy" | Characterization tests + sprout pattern; TDD new code only |
| "No time for tests" | Bug fixes cost 10-100x more in production than development |

---

## Key references

**Books**: Kent Beck *TDD by Example* (2002) • Michael Feathers *Working Effectively with Legacy Code* (2004) • Martin Fowler *Refactoring* (2018)

**Papers**: Nagappan et al. *Realizing quality improvement through TDD* (2008) • Rafique & Misic meta-analysis (2013)

**Tools by language**:
- Python: pytest, hypothesis, coverage.py, mutmut
- JS/TS: vitest, jest, fast-check, stryker
- Java: junit5, mockito, assertj, pitest, jacoco
- Go: testing, testify
- Rust: built-in, proptest, tarpaulin