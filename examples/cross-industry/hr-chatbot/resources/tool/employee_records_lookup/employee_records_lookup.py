"""Employee records lookup tool for the HR chatbot.

Serves employee records from a bundled sample directory, so the tool
always returns a useful, well-formed record without any external
database dependency.
"""

# Sample employee directory backing the lookup tool.
_SAMPLE_EMPLOYEES = {
    "E001": {
        "employee_id": "E001",
        "name": "Alice Johnson",
        "department": "Engineering",
        "role": "Senior Software Engineer",
        "manager": "Priya Raman",
        "leave_balance_days": 14,
    },
    "E002": {
        "employee_id": "E002",
        "name": "Bob Martinez",
        "department": "Human Resources",
        "role": "HR Generalist",
        "manager": "Dana Whitfield",
        "leave_balance_days": 9,
    },
    "E003": {
        "employee_id": "E003",
        "name": "Chen Wei",
        "department": "Finance",
        "role": "Financial Analyst",
        "manager": "Omar Haddad",
        "leave_balance_days": 21,
    },
}


def main(employee_id: str) -> dict:
    """Look up an employee record by employee ID.

    Returns name, department, role, manager, and leave balance for the
    employee from the bundled sample directory.
    """
    employee_id = employee_id.strip().upper()

    record = _SAMPLE_EMPLOYEES.get(employee_id)
    if record is None:
        return {
            "found": False,
            "employee_id": employee_id,
            "source": "demo_directory",
            "message": f"No employee record found for ID {employee_id}.",
        }
    return {"found": True, "source": "demo_directory", **record}
