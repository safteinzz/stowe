//! The remotes level: every remote with where it stands, and below it what a
//! push there would do.

use super::jobs::{Confirm, Target};
use super::load::{self, Reach};
use super::{App, FormFor};
use crate::commands::remote as config;
use crate::mirror::{self, Format};
use crate::remote::remote_format;
use crate::repo::Repo;
use crate::repos::Behind;
use crate::tui::widgets::{Field, Form};

pub(super) struct RemoteRow {
    pub name: String,
    pub url: String,
    pub format: Format,
    pub mount: Option<String>,
    /// The commit it was last known to be at.
    pub last: Option<String>,
    pub reach: Reach,
}

/// The format field's choices, `auto` being the scheme's default.
const FORMATS: &[&str] = &["auto", "mirror", "backup"];

impl App {
    /// Read the remotes from the config, without looking at any of them.
    pub(super) fn load_remotes(&mut self) {
        let repo = Repo::at(self.repo.clone());
        let cfg = repo.config().unwrap_or_default();
        let old = std::mem::take(&mut self.remotes);
        self.remotes = cfg
            .remotes
            .iter()
            .map(|(name, url)| RemoteRow {
                name: name.clone(),
                url: url.clone(),
                format: remote_format(&cfg, name, url),
                mount: cfg.mounts.get(name).cloned(),
                last: repo.remote_head(name).ok().flatten(),
                // What was seen a moment ago stays up while it is looked at again.
                reach: old
                    .iter()
                    .find(|r| &r.name == name)
                    .map_or(Reach::Checking, |r| r.reach),
            })
            .collect();
        self.update_pushed();
    }

    /// Read the remotes again and look at each one, the cursor kept on its row.
    pub(super) fn reread_remotes(&mut self) {
        self.msel.restore = self.remote().map(|r| r.name.clone());
        self.load_remotes();
        for r in &mut self.remotes {
            r.reach = Reach::Checking;
        }
        let cfg = Repo::at(self.repo.clone()).config().unwrap_or_default();
        self.probe_seq += 1;
        load::probe_remotes(self.repo.clone(), &cfg, self.probe_seq, self.tx.clone());
        self.rescope_remotes();
        self.files_for.clear();
        if self.level == super::Level::Remotes {
            self.pending = true;
        }
    }

    /// A look at remote `name` came back. The ref it read off the remote becomes
    /// the record, but never while a job runs: a push records its own head as
    /// it ends, and a ref read before that would put it back.
    pub(super) fn reach_in(&mut self, name: &str, reach: Reach, theirs: Option<String>) {
        let Some(r) = self.remotes.iter_mut().find(|r| r.name == name) else {
            return;
        };
        r.reach = reach;
        let Some(h) = theirs else {
            return;
        };
        if self.job.is_some() || r.last.as_ref() == Some(&h) {
            return;
        }
        if let Err(e) = Repo::at(self.repo.clone()).set_remote_head(name, &h) {
            self.set_failed(format!(
                "could not record where `{name}` is: {}",
                crate::errors::words(&e)
            ));
            return;
        }
        r.last = Some(h);
        self.update_pushed();
        if self.files_for == format!("push:{name}") {
            self.files_for.clear();
            self.pending |= self.level == super::Level::Remotes;
        }
    }

    pub(super) fn rescope_remotes(&mut self) {
        let query = &self.msel.query;
        let visible = (0..self.remotes.len())
            .filter(|&i| {
                let r = &self.remotes[i];
                query.keeps(&format!("{} {} {}", r.name, r.url, r.format.name()))
            })
            .collect();
        self.msel.show(visible);
        let remotes = &self.remotes;
        self.msel.settle_on(|i| remotes[i].name.clone());
    }

    pub(super) fn remote(&self) -> Option<&RemoteRow> {
        self.msel.idx().map(|i| &self.remotes[i])
    }

    /// How far a remote is behind, from the history read so far.
    pub(super) fn behind(&self, r: &RemoteRow) -> Option<Behind> {
        match &r.last {
            None => Some(Behind::Never),
            Some(h) => match self.log.iter().position(|c| &c.hash == h) {
                Some(n) => Some(Behind::By(n)),
                None if self.cfeed.loading => None,
                None => Some(Behind::Elsewhere),
            },
        }
    }

    /// The pane below: what a push to the remote under the cursor would do.
    pub(super) fn enter_remote(&mut self) {
        let Some(name) = self.remote().map(|r| r.name.clone()) else {
            self.files.clear();
            self.fsel.show(Vec::new());
            self.files_for.clear();
            return;
        };
        let key = format!("push:{name}");
        if self.files_for == key {
            return;
        }
        self.files_for = key;
        self.files.clear();
        self.plan_drift = 0;
        self.fsel.show(Vec::new());
        self.plan_failed = None;
        let seq = self.pfeed.issue();
        load::stream_plan(self.repo.clone(), name, seq, self.tx.clone());
    }

    pub(super) fn plan_in(&mut self, rows: Result<(Vec<crate::history::Change>, usize), String>) {
        match rows {
            Ok((rows, drifted)) => {
                self.files = rows;
                self.plan_drift = drifted;
                self.plan_failed = None;
            }
            Err(e) => {
                self.files.clear();
                self.plan_drift = 0;
                self.plan_failed = Some(e);
            }
        }
        self.rescope_files();
    }

    /// `a` on a remote: only a mirror holds files that can change by hand.
    pub(super) fn ask_adapt(&mut self) {
        let Some(r) = self.remote() else {
            return;
        };
        if r.format != Format::Mirror {
            let name = r.name.clone();
            self.set_failed(format!(
                "{name} is a backup: only a mirror has files to adapt"
            ));
            return;
        }
        self.pending_job = Some(super::jobs::Job::Adapt {
            name: r.name.clone(),
            approved: Vec::new(),
        });
    }

    /// `v` on a remote: flip it, after saying which way.
    pub(super) fn ask_convert(&mut self) {
        let Some(r) = self.remote() else {
            return;
        };
        if mirror::local_root(&r.url).is_none() {
            let name = r.name.clone();
            self.set_failed(format!(
                "{name} is in the cloud: only a local remote can be a mirror"
            ));
            return;
        }
        // What is on the drive decides, since a config edited by hand or the
        // form's format pick can say otherwise; an unmounted drive reads empty.
        let now = match mirror::local_root(&r.url).map(|root| mirror::detect_format(&root)) {
            Some(Format::Empty) | None => r.format,
            Some(found) => found,
        };
        let to = match now {
            Format::Mirror => Format::Backup,
            _ => Format::Mirror,
        };
        self.confirm = Some(Confirm::new(Target::Convert {
            name: r.name.clone(),
            to,
        }));
    }

    /// `d` on a remote: the plain gate, since only this repo's record of it goes.
    pub(super) fn ask_forget(&mut self) {
        if let Some(r) = self.remote() {
            self.confirm = Some(Confirm::new(Target::Remove(r.name.clone())));
        }
    }

    pub(super) fn forget_remote(&mut self, name: &str) {
        let at = self.msel.cur;
        let next = self
            .msel
            .visible
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|b| self.msel.visible.get(b)))
            .map(|&i| self.remotes[i].name.clone());
        match config::remove(&Repo::at(self.repo.clone()), name) {
            Ok(()) => self.set_status(format!("forgot {name}; nothing on it was touched")),
            Err(e) => return self.alert(format!("could not forget {name}"), &e),
        }
        self.reread_remotes();
        self.msel.restore = next;
        self.rescope_remotes();
    }

    /// `c`: a form for a new remote.
    pub(super) fn create_remote(&mut self) {
        let form = Form::new("new remote", remote_fields("", "", "auto", ""));
        self.form = Some((form, FormFor::NewRemote));
    }

    /// `e`: the remote under the cursor, every field filled in.
    pub(super) fn edit_remote(&mut self) {
        let Some(r) = self.remote() else {
            return;
        };
        let repo = Repo::at(self.repo.clone());
        let cfg = repo.config().unwrap_or_default();
        let format = cfg.formats.get(&r.name).map_or("auto", String::as_str);
        let form = Form::new(
            format!("edit {}", r.name),
            remote_fields(
                &r.name,
                &crate::paths::short_url(&r.url),
                format,
                r.mount.as_deref().unwrap_or(""),
            ),
        );
        self.form = Some((form, FormFor::EditRemote(r.name.clone())));
    }

    /// The remote form's submit. A refusal leaves the form up under the alert,
    /// so a typo costs one fix rather than the whole form.
    pub(super) fn submit_remote(&mut self, form: &Form, was: Option<&str>) -> bool {
        let repo = Repo::at(self.repo.clone());
        let name = form.value(0).to_string();
        let format = match form.value(2) {
            "auto" => None,
            f => Some(f),
        };
        let mount = Some(form.value(3)).filter(|m| !m.is_empty());
        let saved = (|| -> anyhow::Result<()> {
            let url = config::absolute_url(form.value(1))?;
            // Checked before a rename, which `save` failing after it would
            // leave done under a form that can then never be submitted.
            crate::repo::valid_remote_name(&name)?;
            config::check(&url, format, mount)?;
            match was {
                None if repo.config()?.remotes.contains_key(&name) => {
                    anyhow::bail!("a remote named `{name}` already exists; `e` on it changes it")
                }
                Some(old) if old != name => config::rename(&repo, old, &name)?,
                _ => {}
            }
            config::save(&repo, &name, &url, format, mount)
        })();
        match saved {
            Ok(()) => {
                self.set_status(match was {
                    None => format!("added {name}"),
                    Some(_) => format!("saved {name}"),
                });
                self.reread_remotes();
                self.msel.restore = Some(name);
                self.rescope_remotes();
                true
            }
            Err(e) => {
                self.alert(format!("could not save `{name}`"), &e);
                false
            }
        }
    }
}

fn remote_fields(name: &str, url: &str, format: &str, mount: &str) -> Vec<Field> {
    let mut pick = Field::pick("format", FORMATS);
    pick.placeholder = "auto: a path is a mirror, s3:// a backup";
    pick.value.text = format.to_string();
    vec![
        Field::new("name", "such as drive", true).with(name),
        Field::new("url", "a path, local:/path or s3://bucket/prefix", true).with(url),
        pick,
        Field::new(
            "mount",
            "none · run before each use, such as ~/bin/mount-drive",
            false,
        )
        .with(mount),
    ]
}
