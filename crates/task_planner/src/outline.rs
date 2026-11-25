//! Responsibility: Work out the shape of the to-do list: which group each to-do is in, what it
//! waits for, which to-dos can start now, the deadline each one really has, and the order
//! they are shown in.
//!
//! Pure functions over the open to-dos and their [`TodoItem::group`], [`TodoItem::parent`],
//! [`TodoItem::rank`] and [`TodoItem::after`]. A to-do that is not open (finished, removed,
//! or never there) counts as finished, and a parent that is not an open group counts as no
//! parent, so a list edited by hand or by an older version never blocks anything for good.

use crate::model::TodoItem;
use chrono::NaiveDate;
use std::collections::{BTreeMap, BTreeSet};

/// Responsibility: Answer questions about the shape of one to-do list.
pub struct Outline<'a> {
    todos: &'a [TodoItem],
    index_of: BTreeMap<&'a str, usize>,
    /// The group each to-do sits in (an index into `todos`), after dropping parents that are
    /// missing, not groups, or part of a loop.
    parent_of: Vec<Option<usize>>,
    /// The to-dos in each group (`None`: at the top), in the order they are shown.
    children_of: BTreeMap<Option<usize>, Vec<usize>>,
}

/// Responsibility: Describe one line of the outline as the to-do tab shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineRow<'a> {
    pub todo: &'a TodoItem,
    /// 0 for a to-do at the top, 1 inside one group, and so on.
    pub depth: usize,
}

/// Responsibility: Say what date a to-do must be finished by and where that date comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Due<'a> {
    pub date: NaiveDate,
    /// `None`: the to-do's own deadline. Otherwise the group it is in, or a to-do that waits
    /// for it, whose deadline comes first.
    pub from: Option<&'a TodoItem>,
}

impl<'a> Outline<'a> {
    pub fn new(todos: &'a [TodoItem]) -> Self {
        let index_of: BTreeMap<&str, usize> = todos
            .iter()
            .enumerate()
            .map(|(index, todo)| (todo.id.as_str(), index))
            .collect();
        let mut parent_of: Vec<Option<usize>> = todos
            .iter()
            .enumerate()
            .map(|(index, todo)| {
                todo.parent
                    .as_deref()
                    .and_then(|parent_id| index_of.get(parent_id).copied())
                    .filter(|parent| *parent != index && todos[*parent].group)
            })
            .collect();
        // A loop of parents (possible only in a file edited by hand) is cut where it is found.
        for start in 0..todos.len() {
            let mut seen = BTreeSet::from([start]);
            let mut current = start;
            while let Some(parent) = parent_of[current] {
                if !seen.insert(parent) {
                    parent_of[current] = None;
                    break;
                }
                current = parent;
            }
        }
        let mut children_of: BTreeMap<Option<usize>, Vec<usize>> = BTreeMap::new();
        for (index, parent) in parent_of.iter().enumerate() {
            children_of.entry(*parent).or_default().push(index);
        }
        for children in children_of.values_mut() {
            // By rank; equal ranks keep the order of the list.
            children.sort_by(|left, right| {
                todos[*left]
                    .rank
                    .total_cmp(&todos[*right].rank)
                    .then(left.cmp(right))
            });
        }
        Self {
            todos,
            index_of,
            parent_of,
            children_of,
        }
    }

    pub fn get(&self, todo_id: &str) -> Option<&'a TodoItem> {
        self.index_of.get(todo_id).map(|index| &self.todos[*index])
    }

    /// The group `todo_id` sits in; `None` at the top.
    pub fn parent(&self, todo_id: &str) -> Option<&'a TodoItem> {
        let index = *self.index_of.get(todo_id)?;
        self.parent_of[index].map(|parent| &self.todos[parent])
    }

    /// The groups `todo_id` sits in, outermost first.
    pub fn path(&self, todo_id: &str) -> Vec<&'a TodoItem> {
        let mut path = Vec::new();
        let mut current = self.index_of.get(todo_id).copied();
        while let Some(index) = current.and_then(|index| self.parent_of[index]) {
            path.push(&self.todos[index]);
            current = Some(index);
        }
        path.reverse();
        path
    }

    /// The path as one line: "Thesis › Chapter 2"; empty at the top.
    pub fn path_text(&self, todo_id: &str) -> String {
        self.path(todo_id)
            .iter()
            .map(|group| group.title.as_str())
            .collect::<Vec<_>>()
            .join(" › ")
    }

    /// The to-dos and groups directly in `parent_id` (`None`: at the top), in order.
    pub fn children(&self, parent_id: Option<&str>) -> Vec<&'a TodoItem> {
        let parent = match parent_id {
            Some(parent_id) => match self.index_of.get(parent_id) {
                Some(index) => Some(*index),
                None => return Vec::new(),
            },
            None => None,
        };
        self.children_of
            .get(&parent)
            .map(|children| children.iter().map(|index| &self.todos[*index]).collect())
            .unwrap_or_default()
    }

    /// Every line of the outline, depth first, in order.
    pub fn rows(&self) -> Vec<OutlineRow<'a>> {
        let mut rows = Vec::new();
        self.push_rows(None, 0, &mut rows);
        rows
    }

    fn push_rows(&self, parent: Option<usize>, depth: usize, rows: &mut Vec<OutlineRow<'a>>) {
        for index in self.children_of.get(&parent).into_iter().flatten() {
            rows.push(OutlineRow {
                todo: &self.todos[*index],
                depth,
            });
            self.push_rows(Some(*index), depth + 1, rows);
        }
    }

    /// The to-dos (not groups) inside `group_id`, at any depth, in order.
    pub fn leaves_under(&self, group_id: &str) -> Vec<&'a TodoItem> {
        let Some(group) = self.index_of.get(group_id).copied() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        self.push_rows(Some(group), 0, &mut rows);
        rows.into_iter()
            .map(|row| row.todo)
            .filter(|todo| !todo.group)
            .collect()
    }

    /// `group_id` itself and the groups inside it, at any depth.
    pub fn groups_under(&self, group_id: &str) -> Vec<&'a TodoItem> {
        let Some(group) = self.index_of.get(group_id).copied() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        self.push_rows(Some(group), 0, &mut rows);
        self.get(group_id)
            .into_iter()
            .chain(
                rows.into_iter()
                    .map(|row| row.todo)
                    .filter(|todo| todo.group),
            )
            .collect()
    }

    /// Whatever waits for `todo_id` may start: it is not open (finished, removed, unknown), it
    /// is a routine (a routine is never over, so it never holds anything up), or it is a group
    /// with no unfinished to-do inside.
    pub fn is_finished(&self, todo_id: &str) -> bool {
        match self.get(todo_id) {
            None => true,
            Some(todo) if todo.group => self.leaves_under(todo_id).iter().all(|leaf| leaf.repeat),
            Some(todo) => todo.repeat,
        }
    }

    /// The unfinished to-dos and groups that `todo_id` waits for: its own `after` and that of
    /// every group it is in.
    pub fn blockers(&self, todo_id: &str) -> Vec<&'a TodoItem> {
        let Some(todo) = self.get(todo_id) else {
            return Vec::new();
        };
        let mut blockers: Vec<&TodoItem> = Vec::new();
        for holder in std::iter::once(todo).chain(self.path(todo_id)) {
            for waited_id in &holder.after {
                if waited_id == &holder.id || self.is_finished(waited_id) {
                    continue;
                }
                if let Some(waited) = self.get(waited_id) {
                    if !blockers.iter().any(|known| known.id == waited.id) {
                        blockers.push(waited);
                    }
                }
            }
        }
        blockers
    }

    /// `todo_id` is a to-do (not a group) that waits for nothing unfinished.
    pub fn is_ready(&self, todo_id: &str) -> bool {
        self.get(todo_id).is_some_and(|todo| !todo.group) && self.blockers(todo_id).is_empty()
    }

    /// The to-dos and groups whose own `after` names `todo_id`: what finishing it lets start.
    pub fn waiting_for(&self, todo_id: &str) -> Vec<&'a TodoItem> {
        self.rows()
            .into_iter()
            .map(|row| row.todo)
            .filter(|todo| todo.after.iter().any(|waited_id| waited_id == todo_id))
            .collect()
    }

    /// The date `todo_id` must be finished by: the earliest of its own deadline, the deadlines
    /// of the groups it is in, and those of the to-dos that wait for it (they cannot be done
    /// in time unless it is done first).
    pub fn due(&self, todo_id: &str) -> Option<Due<'a>> {
        let index = *self.index_of.get(todo_id)?;
        self.due_of(index, &mut BTreeSet::new())
    }

    fn due_of(&self, index: usize, visiting: &mut BTreeSet<usize>) -> Option<Due<'a>> {
        if !visiting.insert(index) {
            return None;
        }
        let todo = &self.todos[index];
        let mut due = todo.deadline.map(|date| Due { date, from: None });
        let mut consider = |source: usize, candidate: Option<Due<'a>>| {
            if let Some(candidate) = candidate {
                if due.is_none_or(|current| candidate.date < current.date) {
                    due = Some(Due {
                        date: candidate.date,
                        // Name the to-do the date really belongs to.
                        from: Some(candidate.from.unwrap_or(&self.todos[source])),
                    });
                }
            }
        };
        if let Some(parent) = self.parent_of[index] {
            consider(parent, self.due_of(parent, visiting));
        }
        for (waiter, other) in self.todos.iter().enumerate() {
            if waiter != index && other.after.iter().any(|waited_id| waited_id == &todo.id) {
                consider(waiter, self.due_of(waiter, visiting));
            }
        }
        visiting.remove(&index);
        due
    }

    /// The titles of to-dos that wait for each other in a loop, so that none of them could
    /// ever start; `None` when there is no loop. Groups count as the to-dos inside them.
    pub fn waiting_loop(&self) -> Option<Vec<String>> {
        // Each to-do points at the to-dos it waits for, groups opened up into their to-dos.
        let edges: Vec<Vec<usize>> = (0..self.todos.len())
            .map(|index| {
                let todo = &self.todos[index];
                if todo.group || todo.repeat {
                    return Vec::new();
                }
                let mut targets = BTreeSet::new();
                for holder in std::iter::once(todo).chain(self.path(&todo.id)) {
                    for waited_id in &holder.after {
                        let Some(waited) = self.get(waited_id) else {
                            continue;
                        };
                        let leaves = if waited.group {
                            self.leaves_under(waited_id)
                        } else {
                            vec![waited]
                        };
                        targets.extend(
                            leaves
                                .into_iter()
                                .filter(|leaf| !leaf.repeat)
                                .filter_map(|leaf| self.index_of.get(leaf.id.as_str()).copied()),
                        );
                    }
                }
                targets.into_iter().collect()
            })
            .collect();
        // Depth-first search; a to-do met again while still on the stack closes a loop.
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            New,
            OnStack,
            Done,
        }
        let mut marks = vec![Mark::New; self.todos.len()];
        let mut stack: Vec<usize> = Vec::new();
        fn visit(
            node: usize,
            edges: &[Vec<usize>],
            marks: &mut [Mark],
            stack: &mut Vec<usize>,
        ) -> Option<Vec<usize>> {
            marks[node] = Mark::OnStack;
            stack.push(node);
            for next in &edges[node] {
                match marks[*next] {
                    Mark::OnStack => {
                        let start = stack.iter().position(|entry| entry == next).unwrap_or(0);
                        return Some(stack[start..].to_vec());
                    }
                    Mark::New => {
                        if let Some(found) = visit(*next, edges, marks, stack) {
                            return Some(found);
                        }
                    }
                    Mark::Done => {}
                }
            }
            stack.pop();
            marks[node] = Mark::Done;
            None
        }
        for node in 0..self.todos.len() {
            if marks[node] == Mark::New {
                if let Some(found) = visit(node, &edges, &mut marks, &mut stack) {
                    return Some(
                        found
                            .into_iter()
                            .map(|index| self.todos[index].title.clone())
                            .collect(),
                    );
                }
            }
        }
        None
    }

    /// `group_id` is `todo_id` itself or a group somewhere inside it: moving `todo_id` there
    /// would put it inside itself.
    pub fn is_inside(&self, group_id: &str, todo_id: &str) -> bool {
        group_id == todo_id || self.path(group_id).iter().any(|group| group.id == todo_id)
    }
}
