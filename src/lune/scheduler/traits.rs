use futures_util::Future;
use mlua::prelude::*;

use super::Scheduler;

const ASYNC_IMPL_LUA: &str = r#"
schedule(...)
return yield()
"#;

/**
    Trait for extensions to the [`Lua`] struct, allowing
    for access to the scheduler without having to import
    it or handle registry / app data references manually.
*/
pub(crate) trait LuaSchedulerExt<'lua> {
    /**
        Sets the scheduler for the [`Lua`] struct.
    */
    fn set_scheduler(&'lua self, scheduler: &'lua Scheduler);

    /**
        Creates a function callable from Lua that runs an async
        closure and returns the results of it to the call site.
    */
    fn create_async_function<A, R, F, FR>(&'lua self, func: F) -> LuaResult<LuaFunction<'lua>>
    where
        A: FromLuaMulti<'lua>,
        R: IntoLuaMulti<'lua>,
        F: Fn(&'lua Lua, A) -> FR + 'lua,
        FR: Future<Output = LuaResult<R>> + 'lua;
}

// FIXME: `self` escapes outside of method because we are borrowing `func`
// when we call `schedule_future_thread` in the lua function body below
// For now we solve this by using the 'static lifetime bound in the impl
impl<'lua> LuaSchedulerExt<'lua> for Lua
where
    'lua: 'static,
{
    fn set_scheduler(&'lua self, scheduler: &'lua Scheduler) {
        self.set_app_data(scheduler);
        scheduler.set_interrupt_for(self);
    }

    fn create_async_function<A, R, F, FR>(&'lua self, func: F) -> LuaResult<LuaFunction<'lua>>
    where
        A: FromLuaMulti<'lua>,
        R: IntoLuaMulti<'lua>,
        F: Fn(&'lua Lua, A) -> FR + 'lua,
        FR: Future<Output = LuaResult<R>> + 'lua,
    {
        self.app_data_ref::<&Scheduler>()
            .expect("Lua must have a scheduler to create async functions");

        let async_env = self.create_table_with_capacity(0, 2)?;

        async_env.set(
            "yield",
            self.globals()
                .get::<_, LuaTable>("coroutine")?
                .get::<_, LuaFunction>("yield")?,
        )?;

        async_env.set(
            "schedule",
            LuaFunction::wrap(move |lua: &Lua, args: A| {
                let thread = lua.current_thread();
                let future = func(lua, args);
                let sched = lua
                    .app_data_ref::<&Scheduler>()
                    .expect("Lua struct is missing scheduler");
                sched.spawn_thread(lua, thread, future)?;
                Ok(())
            }),
        )?;

        let async_func = self
            .load(ASYNC_IMPL_LUA)
            .set_name("async")
            .set_environment(async_env)
            .into_function()?;
        Ok(async_func)
    }
}

/**
    Trait for any struct that can be turned into an [`LuaThread`]
    and given to the scheduler, implemented for the following types:

    - Lua threads ([`LuaThread`])
    - Lua functions ([`LuaFunction`])
    - Lua chunks ([`LuaChunk`])
*/
pub trait IntoLuaThread<'lua> {
    /**
        Converts the value into a lua thread.
    */
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>>;
}

impl<'lua> IntoLuaThread<'lua> for LuaThread<'lua> {
    fn into_lua_thread(self, _: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        Ok(self)
    }
}

impl<'lua> IntoLuaThread<'lua> for LuaFunction<'lua> {
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        lua.create_thread(self)
    }
}

impl<'lua, 'a> IntoLuaThread<'lua> for LuaChunk<'lua, 'a> {
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        lua.create_thread(self.into_function()?)
    }
}
