# My Reviews (Claude can ignore these)
## Chunk 2
- in assert_single(), we clone() the given fact. Why not express ownership here and avoid the clone by moving to the entries buffer? The PendingFact is essentially a temporary object more/less, so this seems reasonable? Let me know if I'm missing drawback/benefits of this approach.
- also in assert_single(), we agreed to update the tentative buffer to distinguish commands (e.g. assert vs retract vs...?)
- I notice the use of `?` - this is an idiom/shortcut to return an error result early (and requires the function to already return a compatible Result<> I assume)...
- in `pending_count()` .map() is leveraging the fact that Option is a functor (what is called fmap in Haskell)
- for test utility functions (like `pending_count` and `pending`) is it common to just include directly in the `tests` module (vs. a `tests::utils` module)? also, does Rust support separate _test.rs files? and/or is it just common to include everything in the source file directly? 
- Re: "the prior open fact we'd close is a committed fact in another shard" - it's not _necessarily_ in another shard, right? That may only be the case for external assertions, right (or maybe not even then, since we are currently using the subject as the distinguished shard target)...
- Took some of these notes before reading the discussion (I see you called out `?` operator)
- in highlight 2 you mention "trait borrows so callers don't have to give up ownership" partially noting my concern from earlier, but not answering the question: "why is it beneficial here for callers to NOT give up ownership?" - it seems like rust programmers need a bit of "future vision" to feel whether ownership transfer or cloning are better options. Not a whole lot different from C++ const refs + copy vs move...
- in highlight 4 looks like you address part of my test thoughts. a tests submodule allows private/friend like access for white-box tests without requiring exposure. 
## Chunk 3
- I do think "collapse to last" makes sense for the following reasons: (1) the intermediate values were never observable and (2) since we consider the transaction as a "single point in time" the last change is the only one that should be visible. I'm not quite sure it's a special case, at most it's an optimization that: hides internal details of the transaction and removes facts with empty intervals. Does it make sense to make the change now, or wait until a following chunk?
- So commit currently assumes facts are single cardinality in this chunk; what's the plan for handling this in general? maybe based on the predicate registry?
- Can CommitChange store references (since the lifetime of the facts likely outlive the changes vec?) Right now, they're cloned, but it seems reasonable to return what are effectively const references to the actual fact stored? Any downsides to this vs making a copy?
- good catch on the backdated assert (or just inconsistent valid times). we do need more thinking on this, because there may be different policies to choose from, and real reasons why a caller may need to change the valid intervals. 
## Chunk 4 
- Why are we iterating over the ops vector vs iterating over the last_*_idx maps? It's probably minor, but also seems more direct than doing `continue` where the index doesn't match. We can just use the index to access the op directly. 
- When writing a lambda/closure like what's used in iter().any(...) I notice that |f| is the parameter - is this automatically a borrowed reference (without an `&`?) or is it something else? or is the type (borrowed ref T) inferred from the any() function? Ok - looked at the docs, and it's based on the Self::Item type which is &T for iter()... That seems a little tricky, but I guess you get familiar with the idioms and it's relatively natural to write. 
- I notice some potentially duplicate code that could be abstracted? i.e. could we make a get_bucket(fact) -> bucket from `shard_for` -> `write` -> `entry` or would that not be possible in Rust due to lifetimes? If it's not possible, what options do we have for reducing this kind of duplication?
## Chunk 6 
- trait FactStorage<TFact>
  - assert
  - update
  - retract
  - with_open_facts(subj, pred, Fn(&Vec<TFact>)->())
  - query
  - change handler fns...
  
Query functions/change functions could be separate trait for consistent composition (TripleStore below)
  
- struct InMemoryFactStorage
  - impl FactStorage for InMemoryFactStorage
  
- trait FactTransaction<TFact>
  - assert
  - assert_set
  - assert_indexed
  - retract
  - retract_set
  - retract_indexed
  - commit
  - rollback
  
- trait TripleStore<TFact>
  - new_with_storage(s : FactStorage<TFact>) -> Self
  - new_transaction() -> FactTransaction
  - query(...) -> Iterator (pass through to storage)
  - change handler fns... (pass through to storage)

## Chunk 10

Ok. We need to step back again, here. Something still feels wrong. Duplicating APIs with concrete types to wrap another type with generic type parameters feels broken (what happened to DRY?) If Rust can't enable what we want, then (I'd argue) there is a serious design flaw in Rust (I'm frustrated!)... but before we go that far, let me examine and reiterate my mental model of the system - it's possible there's a non-Rust-familiar flaw in my thinking, though I think most typical architectures would support this model:

- Storage<> models the raw fact storage, and is intended to be pluggable
  - so a concrete implementation (InMemoryStorage<>) can be replaced by a DurableFileStorage<>
  - this implies that Storage should be a dynamic trait?
	- what is the alternative? that the parent concept/trait either embed the type or take a generic bounded type parameter?
- TripleStore<> models the transactional storage and change observability; the architectural model here is that an implementation of TripleStore<> can just use a Storage<> implementation, and doesn't need to know the type directly. Is this a Rust anti-pattern? If not, then wouldn't boxed dyn traits be the way to go?
- Store is a client facing concrete object that implements access control and predicate management (preferrably independently from the TripleStore implementation) and can somehow be configured with an appropriate TripleStore to provide the full Store API. 
- Swapping implementations for these traits really shouldn't require refactoring. 

I guess one question is how dynamic the configuration needs to be... It seems like we _only_ have 2 options: generic type parameters (exposing implementation types somewhere, and essentially forcing static declarations) OR otherwise boxed dyn traits (which limit trait definitions to being dyn-compatible, but allow the dynamic dispatch to fully decouple implementations). 

So - to get the kind of flexibility and decoupling of concerns (I think) I'm aiming for... it seems like Storage<> and TripleStore<> should be made dyn-compatible?

Am I missing another Rust-typical option to enable this flexibility? Or is the flexibility I'm aiming for ill-desired for some reason? What are the tradeoffs? 
