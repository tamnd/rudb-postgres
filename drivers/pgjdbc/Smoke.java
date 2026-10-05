// Connects with pgjdbc, which speaks the protocol itself, and prints the result of SELECT 1.
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.util.Properties;

public class Smoke {
    public static void main(String[] args) throws Exception {
        String url = "jdbc:postgresql://" + System.getenv("GATE_HOST") + ":" + System.getenv("GATE_PORT")
            + "/" + System.getenv("GATE_DATABASE");
        Properties props = new Properties();
        props.setProperty("user", System.getenv("GATE_USER"));
        props.setProperty("password", System.getenv("GATE_PASSWORD"));
        props.setProperty("sslmode", "verify-full");
        props.setProperty("sslrootcert", System.getenv("GATE_ROOT_CERT"));
        try (Connection conn = DriverManager.getConnection(url, props);
             ResultSet rs = conn.createStatement().executeQuery("select 1")) {
            rs.next();
            System.out.println(rs.getInt(1));
        }
    }
}
