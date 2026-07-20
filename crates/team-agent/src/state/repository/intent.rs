use super::StateWriteIntent;

impl<'a> StateWriteIntent<'a> {
    pub(crate) fn launch_team_or_add_agent(
        team_key: &'a str,
        added_agent_id: Option<&'a str>,
    ) -> Self {
        match added_agent_id {
            Some(agent_id) => Self::AddAgent { team_key, agent_id },
            None => Self::LaunchTeam { team_key },
        }
    }
}
