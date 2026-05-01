# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Project Overview

This project aims to build a powerful, dynamic, observable, and low ceremony software programming environment inspired by Smalltalk, Common Lisp, Haskell, moldable development, knowledge bases, and my personal experience developing software in a variety of languages and environments over 27 years. Given the necessary low-level control required for eventually implementing a virtual machine to host the environment, we propose to use Rust (tentatively) for development (also considered C++ (known but frustrating), Zig, Odin, OCaml). 

The project summary can be found in `docs/Project_Summary.md`.

The design principles and some early key decisions are in `docs/Design_Principles_and_Key_Decisions.md`.

The _initial_ high-level component architecture can be found in `docs/High-level_Components.md`.

## Development Record

### Step 1: triple store
The triple store PRD is found in `docs/triple_store_PRD.md`. 

## Preferences and Rules

I will add to this list as the project proceeds, but may also ask you to add things here as well. If something seems like it should be a preference or rule, please suggest putting it here. 

- Do your research and provide references. 
- Do not hallucinate unless otherwise asked for potentially creative solutions.
- Do not be obsequious. Instead, review and accept valid criticism, or form a careful argument for an opposing view or approach if necessary. However, if I make a specific decision, then that decision must be respected. 
- I prefer documentation that clearly states contracts; e.g. a function should clearly specify requirements and what it ensures. requirements and ensure should use code statement if possible, or very clear descriptions otherwise. 
- Documentation for documentation's sake is not necessary, especially since we should be using clear naming.
- We should aim for domain-driven design principles, even though our domain is technically implementation details. i.e. consistent and ubiquitous language, clear bounded contexts, and documentation of these elements for sharing across multiple sessions. 
