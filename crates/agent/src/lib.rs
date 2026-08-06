//! A hierarchical agent over the spreadsheet environment.
//!
//! The arrangement, and the reason for it: a planner decides *what* to do and
//! a compiler decides *where*. The planner sees a summarised observation and
//! returns typed steps naming headers and patterns; the compiler resolves
//! those against the workbook actually in front of it and emits
//! `engine::Action` values.
//!
//! Splitting them is not tidiness. It is what makes the agent checkable:
//!
//! * A plan can be read before it runs. `CreateDerivedColumn { after:
//!   "Price", .. }` says what it will touch; a string of code does not.
//! * A plan survives the sheet moving. Nothing in it is an address, so
//!   inserting a row above the table does not invalidate it.
//! * A failure is attributable to one half or the other, and they are
//!   separately testable.
//!
//! The planner can only say what `plan::Step` can express, and that is the
//! trade being made deliberately: a narrower agent that can be reasoned
//! about, over a general one that cannot.

pub mod compile;
pub mod memory;
pub mod plan;
pub mod policy;
pub mod run;
pub mod validate;

pub use compile::{compile, CompileError, Compiled, Subject};
pub use memory::{MemoPlanner, MicroPolicy, PlanLibrary};
pub use plan::{Aggregate, ColumnRef, FormulaTemplate, Plan, Predicate, RowRange, Step};
pub use policy::{Router, RouterStats, RulePlanner};
pub use run::{run, Ending, Outcome, PlanContext, PlanError, Planner, RunConfig};
pub use validate::{validate, Estimate, Limits, Refusal};
