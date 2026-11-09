# Agents working together

An agent can hand work to other agents: a review by another model, a second opinion, or a task that needs a machine of its own. You ask for it in plain words, and the answers come back to your thread.

## Three ways to hand off

The first is built into the agents. The other two come from the `flotilla` command in every VM, which every agent knows about and when to use:

| Way | Where it runs | Good for |
| --- | --- | --- |
| Subagents | Inside the agent, as Claude Code, Codex and OpenCode do on their own. | Splitting up the agent's own work. |
| `flotilla agent` | Another agent in the same VM, on the same files. It can be a different agent and model. | A review or second opinion from another model, or parallel work that doesn't need its own ports, database or branch. The agents started in one turn answer together, in one message. |
| `flotilla delegate` | A new thread with a VM of its own, its own copy of the repo and its own branch. | Work that needs its own ports, database or checkout, or that runs long. Each one answers on its own. |

An agent in the same VM costs no extra VM. A delegate is a VM like any thread: a start, and its time.

## Ask for it

Say what you want in your prompt; the agent runs the command. For example:

- *Write the median function, then get reviews from Codex on GPT-6.1 Sol and from Claude Code on Fable 5.1, and sum up what they found.*
- *Delegate the database migration to a Codex agent in its own VM, and tell me when it's pushed.*

Unless you name one, the other agent runs the same agent and model as the first. The first agent then ends its turn with a line on what it handed off. When the answers arrive, they come into your thread as a new message, and the agent gets a new turn to tell you what they said.

An agent can also send a follow-up to an agent it started (`flotilla message`) or stop one (`flotilla cancel`). Ask for those in plain words too, such as *ask the reviewer to look at the tests as well*, or *stop the migration*.

## How it looks

Each agent the thread starts gets a card in the thread: its task's title, its agent and model, how many calls it made and how long it took. While it works, the card shows its latest call; open the card for **Asked to**, the task it was given, and its answer.

- A bar under the thread's header lists the agents still running. Click one to find its card.
- When answers arrive, a line such as *2 agents answered* shows above the agent's reply. Click it to see their cards.
- A delegate's card has **Open thread**. Its thread sits under the thread that started it in the sidebar, its first prompt reads *From* and the title of that thread, and its header links back.
- Threads that agents started don't notify you; their answers go to the thread that asked.

![A thread where the agent handed a review to two other agents. Two cards at the top, Codex review of median.py on GPT-5.6 Terra and Haiku review of median.py on Claude Code with Haiku 5.5, each show their calls, time and the start of their answer. Below the agent's reply, a line reads 2 agents answered, followed by its summary of both reviews.](img/thread-day.webp)

Two reviewers in the same VM, and the summary after both answered.

The iPhone app shows the same cards, and opens a delegate's thread from its card.

## Limits

Hand-offs have limits, so an agent can't start a runaway chain of VMs. When a hand-off goes over one, it doesn't start, and the agent is told why.

| Limit | Value |
| --- | --- |
| How deep | 3 hand-offs down from a thread you started. Deeper than that, the agent does the work itself. |
| In one turn | 6 tasks handed off. |
| At once, per thread | 6 agents running in its VM, and 6 of its delegates with their VMs up. |
| At once, in all | 12 delegates with their VMs up under one thread you started, however deep. |
| Since your last message | 20 hand-offs, across the thread you started and every thread handed work from it. |

The last one stops agents from handing work back and forth all night. When it's reached, the thread says *Hand-off limit reached (20 since your last message). Send a message to continue.* Sending a message or a steer in the thread resets it.
