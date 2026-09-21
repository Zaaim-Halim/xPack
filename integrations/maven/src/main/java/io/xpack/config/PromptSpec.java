package io.xpack.config;

/**
 * What an update prompt says, in the publisher's own words.
 *
 * <p>Both fields are shown to a user on their own screen, and they travel
 * inside the signed manifest, so what appears in the dialog is what the
 * publisher signed rather than whatever the update server happened to serve.
 *
 * <p>Left unset, whatever shows the prompt writes its own wording from the
 * application name and the version.
 */
public class PromptSpec {

    private String title;

    private String message;

    public String getTitle() {
        return title;
    }

    public void setTitle(String title) {
        this.title = title;
    }

    public String getMessage() {
        return message;
    }

    public void setMessage(String message) {
        this.message = message;
    }

    public boolean isEmpty() {
        return isBlank(title) && isBlank(message);
    }

    private static boolean isBlank(String value) {
        return value == null || value.isBlank();
    }
}
