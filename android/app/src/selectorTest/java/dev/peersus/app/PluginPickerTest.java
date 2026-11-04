package dev.peersus.app;

import android.test.ActivityInstrumentationTestCase2;
import android.view.accessibility.AccessibilityNodeInfo;
import java.util.List;

/** Selects only an explicitly named synthetic archive through the actual system picker. */
public final class PluginPickerTest extends ActivityInstrumentationTestCase2<MainActivity> {
    public PluginPickerTest() { super(MainActivity.class); }

    public void testSystemPickerDeliversTheChosenArchiveToTheOwner() throws Exception {
        String name = ((android.test.InstrumentationTestRunner)getInstrumentation()).getArguments().getString("archive_name");
        assertNotNull("pass the exact owned archive_name fixture", name);
        assertTrue(name.matches("peersus-selector-[0-9]+\\.tar"));
        setActivityIntent(new android.content.Intent(android.content.Intent.ACTION_MAIN)
            .setClass(getInstrumentation().getTargetContext(), MainActivity.class)
            .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK | android.content.Intent.FLAG_ACTIVITY_CLEAR_TASK));
        MainActivity activity = getActivity();
        org.json.JSONObject before = await(activity, snapshot -> true);
        assertFalse("the reserved synthetic fixture id is already installed", contains(before.getJSONArray("plugins"), "wasm-selector", null));
        android.accessibilityservice.AccessibilityServiceInfo info = getInstrumentation().getUiAutomation().getServiceInfo();
        info.flags |= android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
            | android.accessibilityservice.AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS
            | android.accessibilityservice.AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS;
        getInstrumentation().getUiAutomation().setServiceInfo(info);
        getInstrumentation().runOnMainSync(activity::choosePlugin);
        long deadline = android.os.SystemClock.elapsedRealtime() + 12000;
        boolean chosen = false, documents = false, downloads = false, drawerOpened = false, searchOpened = false, querySet = false;
        while (android.os.SystemClock.elapsedRealtime() < deadline) {
            for (AccessibilityNodeInfo root : roots()) {
                if (root.getPackageName() == null || !root.getPackageName().toString().contains("documentsui")) continue;
                documents = true;
                List<AccessibilityNodeInfo> matches = root.findAccessibilityNodeInfosByText(name);
                for (AccessibilityNodeInfo item : matches) {
                    if (name.contentEquals(item.getText() == null ? "" : item.getText()) && clickArchive(item)) { chosen = true; break; }
                }
                if (!chosen && !downloads) {
                    for (String label : new String[] {"Downloads", "Descărcări", "Descargas"}) {
                        for (AccessibilityNodeInfo item : root.findAccessibilityNodeInfosByText(label)) {
                            if (label.contentEquals(item.getText() == null ? "" : item.getText()) && click(item)) { downloads = true; break; }
                        }
                        if (downloads) break;
                    }
                }
                if (!chosen && !downloads && !drawerOpened) drawerOpened = openRoots(root);
                if (!chosen && !searchOpened && (downloads || !drawerOpened)) {
                    for (AccessibilityNodeInfo search : root.findAccessibilityNodeInfosByViewId("com.google.android.documentsui:id/option_menu_search")) {
                        if (click(search)) { searchOpened = true; break; }
                    }
                }
                if (searchOpened && !querySet) querySet = setQuery(root, name);
                if (chosen) break;
            }
            if (chosen) break;
            Thread.sleep(100);
        }
        if (!chosen) {
            android.os.Bundle observed = new android.os.Bundle();
            observed.putBoolean("systemPickerPresent", documents);
            observed.putBoolean("downloadsNavigationPresent", downloads);
            observed.putInt("accessibleWindowRoots", roots().size());
            observed.putBoolean("searchOpened", searchOpened);
            observed.putBoolean("exactQuerySet", querySet);
            java.util.Set<String> controls = new java.util.TreeSet<>();
            for (AccessibilityNodeInfo root : roots()) {
                if (root.getPackageName() != null && root.getPackageName().toString().contains("documentsui")) controlIds(root, controls);
            }
            observed.putString("systemControlIds", controls.toString());
            getInstrumentation().sendStatus(0, observed);
            getInstrumentation().getUiAutomation().performGlobalAction(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK);
        }
        assertTrue("the exact synthetic archive was not exposed by the system picker", chosen);
        try {
            await(activity, snapshot -> contains(snapshot.getJSONArray("staged"), "wasm-selector", null));
            command(activity, new org.json.JSONObject().put("command", "confirm-install").put("plugin", "wasm-selector"));
            await(activity, snapshot -> contains(snapshot.getJSONArray("plugins"), "wasm-selector", "running"));
            command(activity, new org.json.JSONObject().put("command", "invoke-action").put("plugin", "wasm-selector").put("action", "echo").put("peer", org.json.JSONObject.NULL));
            await(activity, snapshot -> {
                org.json.JSONArray actions = snapshot.getJSONArray("actions");
                for (int index = 0; index < actions.length(); index++) {
                    org.json.JSONObject action = actions.getJSONObject(index);
                    if ("wasm-selector".equals(action.getString("plugin")) && "done".equals(action.getJSONObject("state").getString("state"))) return true;
                }
                return false;
            });
        } finally {
            org.json.JSONObject state = TestFrames.request(activity, new org.json.JSONObject().put("operation", "Snapshot")).getJSONObject("snapshot");
            if (contains(state.getJSONArray("plugins"), "wasm-selector", null)) {
                command(activity, new org.json.JSONObject().put("command", "remove-package").put("plugin", "wasm-selector"));
            } else if (contains(state.getJSONArray("staged"), "wasm-selector", null)) {
                command(activity, new org.json.JSONObject().put("command", "cancel-install").put("plugin", "wasm-selector"));
            }
        }
    }

    private List<AccessibilityNodeInfo> roots() {
        List<AccessibilityNodeInfo> roots = new java.util.ArrayList<>();
        for (android.view.accessibility.AccessibilityWindowInfo window : getInstrumentation().getUiAutomation().getWindows()) {
            AccessibilityNodeInfo root = window.getRoot();
            if (root != null) roots.add(root);
        }
        AccessibilityNodeInfo active = getInstrumentation().getUiAutomation().getRootInActiveWindow();
        if (active != null) roots.add(active);
        return roots;
    }

    private static boolean click(AccessibilityNodeInfo item) {
        AccessibilityNodeInfo clickable = item;
        for (int depth = 0; clickable != null && !clickable.isClickable() && depth < 5; depth++) clickable = clickable.getParent();
        return clickable != null && clickable.isClickable() && clickable.performAction(AccessibilityNodeInfo.ACTION_CLICK);
    }

    private static void controlIds(AccessibilityNodeInfo node, java.util.Set<String> ids) {
        if (node.isClickable() && node.getViewIdResourceName() != null) ids.add(node.getViewIdResourceName());
        for (int index = 0; index < node.getChildCount(); index++) {
            AccessibilityNodeInfo child = node.getChild(index);
            if (child != null) controlIds(child, ids);
        }
    }

    private static boolean setQuery(AccessibilityNodeInfo node, String name) {
        if ("com.google.android.documentsui:id/search_src_text".equals(node.getViewIdResourceName())) {
            android.os.Bundle arguments = new android.os.Bundle();
            arguments.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, name);
            boolean set = node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments);
            if (set) node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.getId());
            return set;
        }
        for (int index = 0; index < node.getChildCount(); index++) {
            AccessibilityNodeInfo child = node.getChild(index);
            if (child != null && setQuery(child, name)) return true;
        }
        return false;
    }

    private static boolean clickArchive(AccessibilityNodeInfo item) {
        for (int depth = 0; item != null && depth < 6; depth++, item = item.getParent()) {
            if ("com.google.android.documentsui:id/item_root".equals(item.getViewIdResourceName())) return click(item);
        }
        return false;
    }

    private static boolean openRoots(AccessibilityNodeInfo node) {
        CharSequence description = node.getContentDescription();
        if (description != null && ("Show roots".contentEquals(description)
                || "Show navigation drawer".contentEquals(description) || "Open navigation drawer".contentEquals(description)
                || "Afișează directoarele rădăcină".contentEquals(description))) {
            return click(node);
        }
        for (int index = 0; index < node.getChildCount(); index++) {
            AccessibilityNodeInfo child = node.getChild(index);
            if (child != null && openRoots(child)) return true;
        }
        return false;
    }

    private interface Condition { boolean matches(org.json.JSONObject snapshot) throws Exception; }

    private static org.json.JSONObject await(android.content.Context context, Condition condition) throws Exception {
        long deadline = android.os.SystemClock.elapsedRealtime() + 15000;
        do {
            try {
                org.json.JSONObject snapshot = TestFrames.request(context, new org.json.JSONObject().put("operation", "Snapshot")).getJSONObject("snapshot");
                if (condition.matches(snapshot)) return snapshot;
            } catch (java.io.IOException unavailable) { }
            Thread.sleep(100);
        } while (android.os.SystemClock.elapsedRealtime() < deadline);
        throw new AssertionError("real owner did not reach the expected extension state");
    }

    private static boolean contains(org.json.JSONArray items, String id, String status) throws Exception {
        for (int index = 0; index < items.length(); index++) {
            org.json.JSONObject item = items.getJSONObject(index);
            if (id.equals(item.getString("id")) && (status == null || status.equals(item.getString("status")))) return true;
        }
        return false;
    }

    private static void command(android.content.Context context, org.json.JSONObject command) throws Exception {
        org.json.JSONObject result = TestFrames.request(context, new org.json.JSONObject().put("operation", "Command").put("command", command));
        assertTrue("real owner refused the synthetic fixture operation", result.getJSONObject("result").has("Ok"));
    }
}
