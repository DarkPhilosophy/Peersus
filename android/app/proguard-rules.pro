# Java names/members in the small platform bridge are resolved dynamically by Rust JNI.
# Keeping the bridge also covers new clipboard callbacks without a fragile second list.
-keep class dev.peersus.app.** { *; }
-keep class android.app.NativeActivity { *; }
-keep class org.rustwindowing.** { *; }
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}
