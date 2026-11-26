package dev.peersus.app;
import dev.peersus.app.IInputRecoveryConsent;
/** No caller-supplied component, user, settings key, command, or permission. */
interface IInputRecovery {
    int reconnect(IInputRecoveryConsent consent) = 0;
    int enable(IInputRecoveryConsent consent) = 1;
    int disable(IInputRecoveryConsent consent) = 2;
    void destroy() = 16777114;
}
