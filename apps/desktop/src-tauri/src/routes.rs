//! Conservative default-output route observations on the common host-sample timeline.
use crate::db::{err, id, Runtime};
use rusqlite::params;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub device: u32,
    pub transport: u32,
    pub rate: u32,
}
impl Snapshot {
    pub fn supported(self) -> bool {
        self.device != 0
            && (self.transport == u32::from_be_bytes(*b"bltn")
                || self.transport == u32::from_be_bytes(*b"usb "))
            && (16000..=192000).contains(&self.rate)
    }
}
pub struct Tracker {
    epoch: String,
    current: Option<(String, Option<Snapshot>, u64)>,
}
impl Tracker {
    pub fn new(epoch: String) -> Self {
        Self {
            epoch,
            current: None,
        }
    }
    pub fn observe(
        &mut self,
        rt: &Runtime,
        meeting: &str,
        at: u64,
        info: Option<Snapshot>,
    ) -> Result<(), String> {
        let db = rt.db()?;
        if let Some((key, previous, last)) = &mut self.current {
            if at < *last {
                return Err("输出路由时间倒退".into());
            }
            if *previous == info {
                db.execute(
                    "UPDATE audio_output_routes SET end_sample=? WHERE id=?",
                    params![at, key.as_str()],
                )
                .map_err(err)?;
                *last = at;
                return Ok(());
            }
            // A switch happened somewhere after the last observation. Do not
            // retroactively declare that unobserved interval valid for either route.
        }
        let key = id();
        let snapshot = info.unwrap_or(Snapshot {
            device: 0,
            transport: 0,
            rate: 0,
        });
        db.execute(
            "INSERT INTO audio_output_routes VALUES (?,?,?,?,?,?,?,?,?)",
            params![
                key,
                meeting,
                self.epoch,
                at,
                at,
                snapshot.device,
                snapshot.transport,
                snapshot.rate,
                snapshot.supported()
            ],
        )
        .map_err(err)?;
        self.current = Some((key, info, at));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tests::MemoryVault;
    use std::sync::Arc;
    #[test]
    fn route_observations_do_not_claim_the_unobserved_switch_interval() {
        let root = std::env::temp_dir().join(format!("helmmeet-route-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        let mid = rt.create("合成路由", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let epoch = id();
        rt.db().unwrap().execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES (?,?,0,'x','recording')",params![epoch,mid]).unwrap();
        let builtin = Snapshot {
            device: 1,
            transport: u32::from_be_bytes(*b"bltn"),
            rate: 48000,
        };
        let usb = Snapshot {
            device: 2,
            transport: u32::from_be_bytes(*b"usb "),
            rate: 48000,
        };
        assert!(builtin.supported());
        assert!(usb.supported());
        assert!(!Snapshot {
            transport: u32::from_be_bytes(*b"blue"),
            ..builtin
        }
        .supported());
        let mut t = Tracker::new(epoch);
        t.observe(&rt, &mid, 0, Some(builtin)).unwrap();
        t.observe(&rt, &mid, 1600, Some(builtin)).unwrap();
        t.observe(&rt, &mid, 3200, Some(usb)).unwrap();
        t.observe(&rt, &mid, 4800, Some(usb)).unwrap();
        let db = rt.db().unwrap();
        let mut q = db
            .prepare(
                "SELECT start_sample,end_sample FROM audio_output_routes ORDER BY start_sample",
            )
            .unwrap();
        let rows = q
            .query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, [(0, 1600), (3200, 4800)]);
        drop(q);
        drop(db);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
