use futures::{Async, Future, Poll};
use futures::task::{self, Task};
use linear_map::LinearMap;

use std::sync::{Arc, Mutex};

type FlagID = u32;

#[derive(Debug, Default)]
struct FlagInner {
    value: bool,
    tasks: LinearMap<u32, Task>,
    available_ids: Vec<FlagID>,
    next_id: FlagID,
}

impl FlagInner {
    fn get_next_id(&mut self) -> FlagID {
        self.available_ids.pop().unwrap_or_else(|| {
            let next_id = self.next_id;
            self.next_id += 1;
            next_id
        })
    }

    fn register_new_task(&mut self, task: Task) -> FlagID {
        let next_id = self.get_next_id();

        self.tasks.insert(next_id, task);

        next_id
    }

    fn discard_task(&mut self, flag_id: FlagID) {
        self.tasks.remove(&flag_id);
        self.available_ids.push(flag_id);
    }
}

#[must_use = "futures must be polled"]
#[derive(Debug, Clone, Default)]
pub struct Flag {
    inner: Arc<Mutex<FlagInner>>,
    task_id: Option<FlagID>,
}

impl Flag {
    pub fn new() -> Flag {
        Flag::default()
    }

    pub fn set(&mut self) {
        let mut inner = self.inner.lock().expect("lock poisoned");
        inner.value = true;

        for (_, task) in &inner.tasks {
            task.notify();
        }
    }

    // pub fn reset(&mut self) {
    //     let mut inner = self.inner.lock().expect("lock poisoned");
    //     inner.value = false;
    // }

    pub fn wrap_future<F, I, E>(
        &self,
        future: F,
        stop_error: E,
    ) -> impl Future<Item = I, Error = E> + 'static
    where
        F: Future<Item = I, Error = E> + 'static,
        I: 'static,
        E: 'static,
    {
        self.clone()
            .then(move |_| Err(stop_error))
            .select(future)
            .map(|(val, _)| val)
            .map_err(|(val, _)| val)
    }
}

impl Future for Flag {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<(), ()> {
        let mut inner = self.inner.lock().expect("lock poisoned");
        if inner.value {
            return Ok(Async::Ready(()));
        }

        if self.task_id.is_none() {
            let task = task::current();
            self.task_id = Some(inner.register_new_task(task));
        }

        Ok(Async::NotReady)
    }
}

impl Drop for Flag {
    fn drop(&mut self) {
        if let Some(task_id) = self.task_id {
            let mut inner = self.inner.lock().expect("lock poisoned");
            inner.discard_task(task_id);
        }
    }
}

pub trait FutureExt: Future + Sized + 'static
where
    Self::Item: 'static,
    Self::Error: 'static,
{
    fn with_flag(
        self,
        flag: Flag,
        stop_error: Self::Error,
    ) -> Box<Future<Item = Self::Item, Error = Self::Error>> {
        Box::new(flag.wrap_future(self, stop_error))
    }
}

impl<F> FutureExt for F
where
    F: Future + 'static,
    F::Item: 'static,
    F::Error: 'static,
{
}
