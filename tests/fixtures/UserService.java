public class UserService {
    private String prefix;

    public UserService(String prefix) {
        this.prefix = prefix;
    }

    public String getUser(String id) {
        return prefix + "-" + id;
    }

    public static void main(String[] args) {
        UserService service = new UserService("user");
        System.out.println(service.getUser("42"));
    }
}
