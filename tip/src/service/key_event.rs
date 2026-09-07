use super::*;
use crate::keyboard;
use weasel_common::message::ContextToken;

pub(super) struct TestedKey {
    token: ContextToken,
    key_up: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> ContextToken {
        ContextToken {
            context_id: 1,
            connection_epoch: 1,
            generation: 1,
        }
    }

    #[test]
    fn win10_test_and_actual_key_process_only_once() {
        let mut pending = None;
        let mut calls = 0;
        // Actual Win10 trace: both repeat count and message time change.
        // Neither is part of callback pairing, deliberately.
        for (test, _lparam, _message_time) in
            [(true, 0x200001u32, 316312), (false, 0x200000, 316359)]
        {
            if !TestedKey::reuse(&mut pending, &token(), false, test) {
                calls += 1;
                pending = TestedKey::processed(token(), false, test, true);
            }
        }
        assert_eq!(calls, 1);
        assert!(pending.is_none());
    }

    #[test]
    fn repeated_tests_reuse_until_actual_callback_for_both_directions() {
        for up in [false, true] {
            let mut pending = TestedKey::processed(token(), up, true, true);
            for _ in 0..3 {
                assert!(TestedKey::reuse(&mut pending, &token(), up, true));
            }
            assert!(TestedKey::reuse(&mut pending, &token(), up, false));
            assert!(pending.is_none());
            assert!(!TestedKey::reuse(&mut pending, &token(), up, false));
        }
    }

    #[test]
    fn consecutive_presses_and_autorepeat_each_process_once() {
        let mut pending = None;
        let mut calls = 0;
        for _ in 0..5 {
            for test in [true, false] {
                if !TestedKey::reuse(&mut pending, &token(), false, test) {
                    calls += 1;
                    pending = TestedKey::processed(token(), false, test, true);
                }
            }
        }
        assert_eq!(calls, 5);
    }

    #[test]
    fn opposite_direction_clears_pending() {
        for up in [false, true] {
            let mut pending = TestedKey::processed(token(), up, true, true);
            assert!(!TestedKey::reuse(&mut pending, &token(), !up, true));
            assert!(pending.is_none());
        }
    }

    #[test]
    fn unhandled_and_actual_keys_do_not_create_pending() {
        assert!(TestedKey::processed(token(), false, true, false).is_none());
        assert!(TestedKey::processed(token(), false, false, true).is_none());
        assert!(!TestedKey::reuse(&mut None, &token(), false, false));
    }

    #[test]
    fn context_generation_and_connection_changes_discard_pending() {
        for changed in [
            ContextToken {
                context_id: 2,
                ..token()
            },
            ContextToken {
                generation: 2,
                ..token()
            },
            ContextToken {
                connection_epoch: 2,
                ..token()
            },
            ContextToken {
                connection_epoch: 0,
                ..token()
            },
        ] {
            let mut pending = TestedKey::processed(token(), false, true, true);
            assert!(!TestedKey::reuse(&mut pending, &changed, false, false));
            assert!(pending.is_none());
        }
    }
}

impl TestedKey {
    // Weasel pairs callbacks by direction, not Windows message identity.
    // Opposite-direction callbacks clear pending state; repeated Test callbacks
    // keep it, while the actual Key callback consumes it. One Option therefore
    // represents the two mutually exclusive down/up pending flags.
    fn reuse(pending: &mut Option<Self>, token: &ContextToken, key_up: bool, test: bool) -> bool {
        let Some(previous) = pending.take() else {
            return false;
        };
        if previous.token != *token || token.connection_epoch == 0 || previous.key_up != key_up {
            return false;
        }
        if test {
            *pending = Some(previous);
        }
        true
    }

    fn processed(token: ContextToken, key_up: bool, test: bool, eaten: bool) -> Option<Self> {
        (test && eaten && token.connection_epoch != 0).then_some(Self { token, key_up })
    }
}

impl TextService {
    pub(super) fn forward_key_event(
        &self,
        context: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
        key_up: bool,
        test: bool,
        session: IUnknown,
    ) -> Result<BOOL> {
        if !self.activated.load(Ordering::Acquire) {
            self.lock(&self.tested_key)?.take();
            return Ok(BOOL(0));
        }
        // Let the shell see our explicit Win+. action, not the Rime engine.
        if keyboard::is_emoji_shortcut() {
            self.lock(&self.tested_key)?.take();
            return Ok(BOOL(0));
        }
        let Some(context) = context.to_owned() else {
            self.lock(&self.tested_key)?.take();
            return Ok(BOOL(0));
        };
        let state = self.ensure_context(context, &session)?;
        if !state.alive.load(Ordering::Acquire) || state.suspended.load(Ordering::Acquire) {
            self.lock(&self.tested_key)?.take();
            return Ok(BOOL(0));
        }
        self.focus_context(Some(state.clone()))?;
        if self.cleanup_disconnected_composition(&state, &session)? {
            return Ok(BOOL(0));
        }
        let token = state.token()?;
        weasel_common::input_trace!(
            "key.identity context={} vk={} lp={} up={} test={} message_time={}",
            state.id,
            wparam.0,
            lparam.0,
            key_up,
            test,
            unsafe { bindings::GetMessageTime() }
        );
        {
            let mut cached = self.lock(&self.tested_key)?;
            if TestedKey::reuse(&mut cached, &token, key_up, test) {
                weasel_common::input_trace!(
                    "key.pending_hit context={} up={} test={} consumed={}",
                    state.id,
                    key_up,
                    test,
                    !test
                );
                return Ok(BOOL(1));
            }
        }
        let mut event = keyboard::translate(wparam.0 as u32, lparam.0 as i64, key_up);
        if event.keycode.is_none() {
            return Ok(BOOL(0));
        }
        event.token = Some(token);
        let response = self.lock(&state.rpc)?.process_key_event(event);
        let Some(response) = response else {
            weasel_common::input_trace!(
                "key.fallback context={} test={} reason=no_response",
                state.id,
                test
            );
            // The out-of-process server is optional during activation.  Let
            // the host handle the key as ordinary text and retry connection
            // on the next input event.
            self.refresh_language_bar()?;
            return Ok(BOOL(0));
        };
        let eaten = response.eaten;
        weasel_common::input_trace!(
            "key.direct_reply token={:?} revision={} eaten={} test={}",
            response.token,
            response.revision,
            eaten,
            test
        );
        // Connection may have been established during this request. Use the
        // response epoch, and never retain a result from a superseded session.
        if let Some(token) = response.token.as_ref() {
            if state.matches(Some(token))? {
                *self.lock(&self.tested_key)? =
                    TestedKey::processed(token.clone(), key_up, test, eaten);
            }
        }

        // Responses, including this one, are delivered on one ordered stream.
        // Do not apply a direct reply ahead of a queued renderer commit.
        self.drain_context_updates(&session)?;
        Ok(BOOL(eaten as i32))
    }
}
