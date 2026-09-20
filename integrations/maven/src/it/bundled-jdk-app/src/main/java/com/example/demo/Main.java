package com.example.demo;

import java.awt.BorderLayout;
import java.awt.GraphicsEnvironment;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import javax.swing.JFrame;
import javax.swing.JLabel;
import javax.swing.SwingUtilities;

/** A deliberately ordinary desktop application, used to prove xPack launches one. */
public final class Main {

    public static void main(String[] args) throws Exception {
        System.out.println("demo: args=" + Arrays.toString(args));
        System.out.println("demo: java.home=" + System.getProperty("java.home"));
        System.out.println("demo: java.version=" + System.getProperty("java.version"));
        System.out.println("demo: user.dir=" + System.getProperty("user.dir"));
        System.out.println("demo: DEMO_GREETING=" + System.getenv("DEMO_GREETING"));
        System.out.println("demo: XPACK_APPLICATION_DIR=" + System.getenv("XPACK_APPLICATION_DIR"));

        if (Arrays.asList(args).contains("--crash")) {
            System.out.println("demo: exiting non-zero on purpose");
            System.exit(3);
        }

        reportHealthy();

        if (Arrays.asList(args).contains("--gui") && !GraphicsEnvironment.isHeadless()) {
            showWindow();
            Thread.sleep(Long.getLong("demo.lifetime", 4000L));
            System.out.println("demo: closing");
            System.exit(0);
        }

        Thread.sleep(Long.getLong("demo.lifetime", 1500L));
        System.out.println("demo: done");
    }

    /** Creates the file xPack named, which is the one health signal that is not an inference. */
    private static void reportHealthy() {
        String health = System.getenv("XPACK_HEALTH_FILE");
        if (health == null) {
            System.out.println("demo: no XPACK_HEALTH_FILE in the environment");
            return;
        }
        try {
            Files.createDirectories(Path.of(health).getParent());
            Files.writeString(Path.of(health), "ok\n");
            System.out.println("demo: wrote health file " + health);
        } catch (IOException e) {
            System.out.println("demo: could not write health file: " + e);
        }
    }

    private static void showWindow() throws Exception {
        SwingUtilities.invokeAndWait(() -> {
            JFrame frame = new JFrame("xPack demo");
            frame.setDefaultCloseOperation(JFrame.EXIT_ON_CLOSE);
            frame.add(new JLabel("Launched by xPack, on " + System.getProperty("java.version")),
                      BorderLayout.CENTER);
            frame.setSize(420, 140);
            frame.setLocationRelativeTo(null);
            frame.setVisible(true);
            System.out.println("demo: window shown");
        });
    }
}
