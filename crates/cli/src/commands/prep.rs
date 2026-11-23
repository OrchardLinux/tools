use super::{plan_session, session_to_start, Way};
use crate::args::Global;
use crate::output::{self, Output};
use orchard_boards::catalog::Catalog;
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_device::prep;
use std::time::Duration;

pub fn run(global: &Global, way: &Way, plan: bool, timeout: Duration) -> Result<Output> {
    let catalog = Catalog::open(&global.data_dir()?)?;
    if plan {
        let session = plan_session(global, way.path)?;
        return Ok(prep::plan(&way.request(&catalog, None), session.as_ref())?.into());
    }
    let store = Store::open(global.cache.as_deref())?;
    let request = way.request(&catalog, Some(&store));
    let mut session = session_to_start(global, timeout)?;
    let went = prep::run(&mut session, &request, &mut |step| {
        output::progress(global, step)
    })?;
    Ok(went.into())
}
