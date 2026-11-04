Working through my thoughts on Smol's concurrent "theory", relationship to scopes and contexts, and how principled and disciplined we need to be.

Wikipedia asserts that "scope" primarily has to do with names (though it's not quite clear on what authority, primarily convention?), while context is better related to execution or runtime concerns. So, we will try to use the terms more carefully in our discussion. 

For our purposes, the most applicable dictionary definition of context is 
> the [situation](https://dictionary.cambridge.org/us/dictionary/english/situation "situation") within which something [exists](https://dictionary.cambridge.org/us/dictionary/english/exist "exists") or [happens](https://dictionary.cambridge.org/us/dictionary/english/happen "happens"), and that can [help](https://dictionary.cambridge.org/us/dictionary/english/help "help") [explain](https://dictionary.cambridge.org/us/dictionary/english/explain "explain") it

i.e. there is some (perhaps implicit) situation that provides supporting information for some event or activity, and may have an effect on an event or activities behavior. 

(Need defs for `event` and `activity`)

To be a little more specific, contexts are always "bounded" in some way with respect to the events of a system. I.e. given a single context C and a set of activities in a system at time t, there are some activities that can be considered "part of" or "inside" some context C and other activities that are "outside" context C. Therefore, every context has an originating event that can be used to determine its bounds. 

If every context has an origination, does every context have a termination? Not necessarily, so we further distinguish two kinds of context: terminating and non-terminating. A simple way to think about a terminating context is that there is a bounded time interval that the context was "active." A non-terminating context has an unbounded time interval (it "started" at some point, but does not have an explicit end). 

There are a bunch of different kinds of context (some correspond to a scope as well) we may be interested in. This is not a new concept, and is applicable to many programming languages and paradigms. 

Some contexts:
- lexical (file, function, struct, block)
- namespace
- lifetime (entity, object, thread, resource?)
- program
- ownership
- transaction
- condition/exception handler
- dynamic variable/constant
- resource (is it separate from lifetime?)
- causal

(It seems like many contexts are just different flavors of another context)

Some observations:
- Transactions should probably be more explicit (more about this later). If not for implementation, at least for better design and communication
	- more specific and explicit about the entities that participate
	- tx implementations as a subclass
- condition handlers: no function or method really cares about the handler context! this is no different, really, than try/catch blocks in other exception-based languages. but we need it to help make programs work. 
- Smol can be thought of as being based on "turns" (to use terminology from E - is there other terminology that could be used) - where a turn is an entity's processing/computation of a single message. in the discussion above, a "turn" is a kind of activity, a message send is an event that causes a turn. 
- Therefore, Smol can be described by a causal graph of turns, where turns are nodes and events like message sends are edges. 
- Like any program, there is one root turn, everything else is caused by that (so, that is a likely example of a non-terminating context)
- In Smol, every send returns a Future. So this is one reason why message sends are not like "go statements" - since there is a flow, directly connected to the turn activity, back to the sender. The sender can choose to process this or not. 
- Every send is causal, by necessity. 
- Can causal chains intersect? Hypothesis: no. Maybe it depends on the definition of "intersect". But this is the reason that contexts are bounded... 
- Therefore: causal chains are a reasonable surrogate for call stacks in sequential languages. 

In causal graphs, what are the possible edges?
- message send: A sends a message M to B; B eventually executes a turn. B's turn was caused by the message from A. 
- entity spawn: A creates a new entity B, so B's initialization turn is caused by A. 
- "registration": this one is controversial, and caused some of the previous discussions and hair pulling. The core concept is that A sends a message to B _along with a block C that may get executed later_. B or the runtime _may_ execute C. An example is a Timer that will run a block after some specified period unless cancelled before the block runs. To be consistent, if C executes, A _caused_ it to run by sending the message that requested B or the runtime to execute C. Therefore, C continues the causal chain where A sent the message. 
	- Is this _just_ a special case of a "message send"? Maybe, but the mechanism may be different enough to call out as a separate edge? 
	- I also think `then:orElse:` is, at least conceptually, another kind of registration, and doesn't need another edge type? 
		- I am a little unclear on how the `then:orElse` block is currently implemented. Is it attached to the future somehow or inserted as another future/continuation wrapping the future it's attached to? at the "syntax" level, it's just a message send to the future, which should mean that it just synchronously waits for the future to resolve (or reject) and then executes the correct arm. 

(doesn't seem like there are any other edges - proof?)

If a node in a causal graph is a turn, how do we define a leaf (terminal) node when computation is done by message sends? It is a turn in an entity that does not send any messages to another _non-owned entity_. Leaf nodes are crucial for terminating contexts. 

How does a context not terminate? There are two cases:
1. Deep: true or mutual unconditional recursion or every scheduled block unconditionally schedules another 
2. Wide: infinite loop of sends to an entity

Smol currently handles self-recursion with tail-call optimization. 
How does Smol currently handle mutual recursion? I.e. if A calls B, and B calls A, do we get deadlock if A and B are separate entities? Do we need something different?